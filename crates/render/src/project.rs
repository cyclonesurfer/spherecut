//! The projection pass (M1 plan section 4.3): a slot, seen through a
//! view, into a linear `Rgba16Float` target; and its flat twin, the GUI's
//! raw view of a source that isn't equirect.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use glam::{DMat3, DQuat, DVec2, DVec3};
use spherecut_sphere::equirect;
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
    /// What the target shows: the lens's picture, or the whole sphere.
    pub out: OutKind,
    /// Where the lens's frame is in the target; `None` is all of it.
    pub framing: Option<Framing>,
}

/// What a view draws into its target (M2 plan section 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutKind {
    /// The lens's picture, its frame `p.x = ±1` at the frame's sides.
    #[default]
    Lens,
    /// The whole sphere as equirect: pixel `(x, y)` of a `w × h` target
    /// is the direction at longitude `(x + ½)/w · 2π − π` and latitude
    /// `π/2 − (y + ½)/h · π`, turned by the orientation. The lens goes
    /// unused.
    Equirect,
}

/// The output frame inside a larger target (M2 plan section 7.1): the
/// reframer's viewer draws the output's frame as `rect` and the same
/// lens past its edges, multiplied by `dim`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Framing {
    /// Left, top, width and height in pixels.
    pub rect: [f64; 4],
    /// In linear light, what is past the frame is multiplied by this.
    pub dim: f64,
}

impl ViewParams {
    pub fn new(orientation: DQuat, lens: Lens) -> ViewParams {
        ViewParams {
            orientation,
            lens,
            out: OutKind::Lens,
            framing: None,
        }
    }

    /// The whole sphere as equirect, turned by `orientation`.
    pub fn equirect(orientation: DQuat) -> ViewParams {
        ViewParams {
            out: OutKind::Equirect,
            ..ViewParams::new(
                orientation,
                Lens::new(Projection::Rectilinear, 90f64.to_radians()),
            )
        }
    }

    /// This view with its frame at `framing` in the target.
    pub fn framed(self, framing: Framing) -> ViewParams {
        ViewParams {
            framing: Some(framing),
            ..self
        }
    }

    /// The frame's rectangle in a `size` target: `framing`'s, or the
    /// whole target.
    pub fn frame(&self, size: (u32, u32)) -> [f64; 4] {
        self.framing
            .map_or([0.0, 0.0, f64::from(size.0), f64::from(size.1)], |f| f.rect)
    }

    /// The lens point at `(x, y)` of a `size` target, in pixels from the
    /// top-left corner: [`pixel::to_lens`] over the frame.
    pub fn lens_point(&self, size: (u32, u32), x: f64, y: f64) -> DVec2 {
        let [fx, fy, fw, fh] = self.frame(size);
        DVec2::new(
            2.0 * (x - fx) / fw - 1.0,
            (1.0 - 2.0 * (y - fy) / fh) * fh / fw,
        )
    }

    /// The world direction through `(x, y)` of a `size` target.
    pub fn dir(&self, size: (u32, u32), x: f64, y: f64) -> DVec3 {
        let d = match self.out {
            OutKind::Lens => self.lens.unproject(self.lens_point(size, x, y)),
            OutKind::Equirect => {
                equirect::to_dir(DVec2::new(x / f64::from(size.0), y / f64::from(size.1)))
            }
        };
        (self.orientation * d).normalize()
    }

    /// Where in a `size` target the world direction `d` is shown, the
    /// inverse of [`Self::dir`]; `None` where the lens can't show it.
    pub fn pixel_of(&self, size: (u32, u32), d: DVec3) -> Option<DVec2> {
        let v = self.orientation.inverse() * d;
        match self.out {
            OutKind::Lens => {
                let p = self.lens.project_morph(v)?;
                let [fx, fy, fw, fh] = self.frame(size);
                Some(DVec2::new(
                    fx + (p.x + 1.0) * fw / 2.0,
                    fy + (fh - p.y * fw) / 2.0,
                ))
            }
            OutKind::Equirect => {
                let uv = equirect::from_dir(v);
                Some(DVec2::new(
                    uv.x * f64::from(size.0),
                    uv.y * f64::from(size.1),
                ))
            }
        }
    }

    /// The angle from the lens's axis at `(x, y)`: 0 for equirect.
    pub fn theta(&self, size: (u32, u32), x: f64, y: f64) -> f64 {
        match self.out {
            OutKind::Lens => self.lens.theta(self.lens_point(size, x, y).length()),
            OutKind::Equirect => 0.0,
        }
    }

    /// What linear light at `(x, y)` is multiplied by: the framing's dim
    /// past the frame, else 1.
    pub fn dim_at(&self, x: f64, y: f64) -> f64 {
        match (self.out, self.framing) {
            (OutKind::Lens, Some(f)) => {
                let [fx, fy, fw, fh] = f.rect;
                let inside = (fx..=fx + fw).contains(&x) && (fy..=fy + fh).contains(&y);
                if inside { 1.0 } else { f.dim }
            }
            _ => 1.0,
        }
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

/// The projection shader's two fragment stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Pass {
    /// `fs_project`: through the view's lens.
    Project,
    /// `fs_flat`: the picture as coded.
    Flat,
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
    /// 0 the lens, 1 equirect ([`OutKind`]).
    pub out_kind: u32,
    /// The lens's frame in the target: left, top, width, height.
    pub frame: [f32; 4],
    pub dim: f32,
    pub _pad: [f32; 3],
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
            out_kind: match view.out {
                OutKind::Lens => 0,
                OutKind::Equirect => 1,
            },
            frame: view.frame(size).map(|v| v as f32),
            dim: view.framing.map_or(1.0, |f| f.dim) as f32,
            _pad: [0.0; 3],
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

    /// A view of the whole texture, for passes that read it.
    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.view
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
    pipelines: Mutex<HashMap<(Filter, DebugMode, Pass), wgpu::RenderPipeline>>,
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
        self.pipeline(filter, debug, Pass::Project);
    }

    /// Builds the flat pass's pipeline for `filter` now.
    pub fn prepare_flat(&self, filter: Filter) {
        self.pipeline(filter, DebugMode::Off, Pass::Flat);
    }

    fn pipeline(&self, filter: Filter, debug: DebugMode, pass: Pass) -> wgpu::RenderPipeline {
        let mut map = self
            .pipelines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry((filter, debug, pass))
            .or_insert_with(|| {
                let (label, entry) = match pass {
                    Pass::Project => ("project", "fs_project"),
                    Pass::Flat => ("flat", "fs_flat"),
                };
                layouts::fullscreen_pipeline(
                    &self.gpu.device,
                    label,
                    &self.module,
                    entry,
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
        let pipeline = self.pipeline(filter, debug, Pass::Project);
        let wraps = slot.wraps.load(Ordering::Acquire);
        self.draw(enc, slot, target, &pipeline, wraps, filter, timers, None);
    }

    /// [`Self::record`] of `rows` of `target`'s extent alone, its other
    /// rows left as they were: a large view drawn in bands, a submission
    /// each, so that a window that shares the queue waits behind one band
    /// of it, not the whole (M2 plan item 22). The bands of one view draw
    /// what one [`Self::record`] would.
    #[allow(clippy::too_many_arguments)]
    pub fn record_rows(
        &self,
        enc: &mut wgpu::CommandEncoder,
        slot: &GpuFrame,
        view: &ViewParams,
        target: &LinearTarget,
        filter: Filter,
        debug: DebugMode,
        rows: Range<u32>,
    ) -> Result<(), RenderError> {
        self.gpu.check()?;
        let (w, h) = target.extent;
        if rows.start >= rows.end || rows.end > h {
            return Err(RenderError::Invalid(format!(
                "rows {rows:?} of a {w}x{h} target"
            )));
        }
        let uniform = ViewUniform::new(view, (w, h));
        self.gpu
            .queue
            .write_buffer(&target.view_uniform, 0, bytemuck::bytes_of(&uniform));
        let pipeline = self.pipeline(filter, debug, Pass::Project);
        let wraps = slot.wraps.load(Ordering::Acquire);
        self.draw(
            enc,
            slot,
            target,
            &pipeline,
            wraps,
            filter,
            None,
            Some(rows),
        );
        Ok(())
    }

    /// Records `slot`'s picture flat into `target`'s extent, scaled to
    /// fill it: the GUI's raw view, which keeps the picture's aspect by
    /// the extent it gives. Unchecked, as
    /// [`Self::record_unchecked`]; one draw per target per submission.
    pub fn record_flat_unchecked(
        &self,
        enc: &mut wgpu::CommandEncoder,
        slot: &GpuFrame,
        target: &LinearTarget,
        filter: Filter,
        timers: Option<&mut GpuTimers>,
    ) {
        let (w, h) = target.extent;
        let uniform = ViewUniform {
            size: [w as f32, h as f32],
            frame: [0.0, 0.0, w as f32, h as f32],
            dim: 1.0,
            ..ViewUniform::default()
        };
        self.gpu
            .queue
            .write_buffer(&target.view_uniform, 0, bytemuck::bytes_of(&uniform));
        let pipeline = self.pipeline(filter, DebugMode::Off, Pass::Flat);
        // Clamped at the picture's edges: its sides don't meet.
        self.draw(enc, slot, target, &pipeline, false, filter, timers, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        enc: &mut wgpu::CommandEncoder,
        slot: &GpuFrame,
        target: &LinearTarget,
        pipeline: &wgpu::RenderPipeline,
        wraps: bool,
        filter: Filter,
        timers: Option<&mut GpuTimers>,
        rows: Option<Range<u32>>,
    ) {
        let (w, h) = target.extent;
        let sampler =
            &self.samplers[usize::from(wraps)][usize::from(filter == Filter::Anisotropic)];
        let mut timers = timers;
        let span = timers.as_deref_mut().and_then(|t| t.begin(enc));
        {
            let mut pass = match &rows {
                None => layouts::fullscreen_pass(enc, "project", &target.view),
                Some(_) => layouts::fullscreen_pass_over(enc, "project rows", &target.view),
            };
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            if let Some(r) = &rows {
                pass.set_scissor_rect(0, r.start, w, r.end - r.start);
            }
            pass.set_pipeline(pipeline);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frames_sides_are_the_lens_edges() {
        let v = ViewParams::new(DQuat::IDENTITY, Lens::new(Projection::Rectilinear, 1.0)).framed(
            Framing {
                rect: [30.0, 10.0, 90.0, 160.0],
                dim: 0.4,
            },
        );
        let size = (200, 180);
        assert_eq!(v.lens_point(size, 30.0, 90.0), DVec2::new(-1.0, 0.0));
        assert_eq!(
            v.lens_point(size, 120.0, 10.0),
            DVec2::new(1.0, 160.0 / 90.0)
        );
        assert_eq!(v.lens_point(size, 75.0, 90.0), DVec2::ZERO);
        // Past the frame the lens goes on, dimmed.
        assert_eq!(v.lens_point(size, 165.0, 90.0), DVec2::new(2.0, 0.0));
        assert_eq!((v.dim_at(75.0, 90.0), v.dim_at(150.0, 90.0)), (1.0, 0.4));
        // Unframed, the frame is the target.
        let plain = ViewParams { framing: None, ..v };
        assert_eq!(plain.frame(size), [0.0, 0.0, 200.0, 180.0]);
        assert_eq!(
            plain.lens_point(size, 13.0, 47.0),
            spherecut_sphere::pixel::to_lens(size, 13.0, 47.0)
        );
        assert_eq!(plain.dim_at(-5.0, 0.0), 1.0);
        // `pixel_of` undoes `dir`, framed or not.
        for view in [v, plain] {
            for (x, y) in [(31.0, 12.0), (75.0, 90.0), (160.0, 170.0)] {
                let back = view.pixel_of(size, view.dir(size, x, y)).unwrap();
                assert!(back.abs_diff_eq(DVec2::new(x, y), 1e-9), "{back}");
            }
        }
    }

    #[test]
    fn equirect_output_is_the_sphere_turned() {
        let size = (360, 180);
        let v = ViewParams::equirect(DQuat::IDENTITY);
        // The middle is forward, the left edge behind, the top the zenith.
        assert!(v.dir(size, 180.0, 90.0).abs_diff_eq(DVec3::NEG_Z, 1e-12));
        assert!(v.dir(size, 0.0, 90.0).abs_diff_eq(DVec3::Z, 1e-12));
        assert!(v.dir(size, 180.0, 0.0).abs_diff_eq(DVec3::Y, 1e-12));
        // Turned 90° right, the middle looks east (+X).
        let q = DQuat::from_rotation_y(-std::f64::consts::FRAC_PI_2);
        let east = ViewParams::equirect(q).dir(size, 180.0, 90.0);
        assert!(east.abs_diff_eq(DVec3::X, 1e-12), "{east}");
        assert_eq!(v.theta(size, 3.0, 4.0), 0.0);
        assert_eq!(
            v.framed(Framing {
                rect: [0.0; 4],
                dim: 0.0
            })
            .dim_at(1.0, 1.0),
            1.0
        );
        let u = ViewUniform::new(&v, size);
        assert_eq!(
            (u.out_kind, u.frame, u.dim),
            (1, [0.0, 0.0, 360.0, 180.0], 1.0)
        );
        let back = ViewParams::equirect(q).pixel_of(size, east).unwrap();
        assert!(back.abs_diff_eq(DVec2::new(180.0, 90.0), 1e-9), "{back}");
    }
}
