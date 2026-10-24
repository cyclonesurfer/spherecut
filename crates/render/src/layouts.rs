//! The shader sources and the bind group layouts they share. wgpu
//! deduplicates identical layouts, so each user makes its own from these
//! functions and they stay compatible.

/// What every pass starts with: the full-screen triangle and `round_f16`,
/// which a Tier 1 test runs on its own.
pub const COMMON_WGSL: &str = include_str!("shaders/common.wgsl");
/// The projection pass, `shaders/common.wgsl` then `shaders/project.wgsl`.
pub const PROJECT_WGSL: &str = concat!(
    include_str!("shaders/common.wgsl"),
    include_str!("shaders/project.wgsl")
);
/// The mip pass.
pub const MIPS_WGSL: &str = concat!(
    include_str!("shaders/common.wgsl"),
    include_str!("shaders/mips.wgsl")
);
/// The ingest pass for deep planes.
pub const INGEST_WGSL: &str = concat!(
    include_str!("shaders/common.wgsl"),
    include_str!("shaders/ingest.wgsl")
);
/// The encode pass (sRGB OETF).
pub const ENCODE_WGSL: &str = concat!(
    include_str!("shaders/common.wgsl"),
    include_str!("shaders/encode.wgsl")
);

pub(crate) fn module(device: &wgpu::Device, label: &str, source: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn uniform(binding: u32, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    }
}

fn texture(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

const FILTERABLE: wgpu::TextureSampleType = wgpu::TextureSampleType::Float { filterable: true };
const UNFILTERED: wgpu::TextureSampleType = wgpu::TextureSampleType::Float { filterable: false };

/// Projection group 0: the view.
pub(crate) fn view(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("view"),
        entries: &[uniform(0, size_of::<crate::project::ViewUniform>() as u64)],
    })
}

/// Projection group 1: a slot's source uniform and its three planes.
pub(crate) fn source(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("source"),
        entries: &[
            uniform(0, size_of::<crate::color::SourceUniform>() as u64),
            texture(1, FILTERABLE),
            texture(2, FILTERABLE),
            texture(3, FILTERABLE),
        ],
    })
}

/// Projection group 2: the sampler.
pub(crate) fn sampler(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sampler"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        }],
    })
}

/// The mip pass: the level above, read with `textureLoad`.
pub(crate) fn mip_input(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mip input"),
        entries: &[texture(0, UNFILTERED)],
    })
}

/// The ingest pass: integer codes and their shift.
pub(crate) fn ingest_input(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ingest input"),
        entries: &[
            texture(0, wgpu::TextureSampleType::Uint),
            uniform(1, size_of::<crate::upload::IngestUniform>() as u64),
        ],
    })
}

/// The encode pass: the linear target and where it sits.
pub(crate) fn encode_input(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("encode input"),
        entries: &[
            texture(0, UNFILTERED),
            uniform(1, size_of::<crate::encode::PlaceUniform>() as u64),
        ],
    })
}

/// A pipeline drawing [`crate`]'s full-screen triangle into one target of
/// `format`, with `fragment` from `module` and its overrides.
pub(crate) fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    fragment: &str,
    constants: &[(&str, f64)],
    groups: &[&wgpu::BindGroupLayout],
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let groups: Vec<Option<&wgpu::BindGroupLayout>> = groups.iter().map(|g| Some(*g)).collect();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &groups,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_full"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// One draw of the full-screen triangle into `target`, cleared first.
pub(crate) fn fullscreen_pass<'e>(
    enc: &'e mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}
