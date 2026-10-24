//! Frames into slots (M1 plan section 4.4): plane bytes into level 0,
//! deep planes through the ingest pass, then the mip chain, in one
//! submission per frame that never waits on the GPU.

use std::collections::HashMap;
use std::time::Instant;

use spherecut_core::video::{PlaneData, VideoPlanes};

use crate::color::Placement;
use crate::error::RenderError;
use crate::frame::GpuFrame;
use crate::gpu::Gpu;
use crate::layouts;
use crate::plane::{PlaneFormat, SlotShape};
use crate::timers::GpuTimers;

/// The ingest pass's uniform (`shaders/ingest.wgsl`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct IngestUniform {
    pub shift: u32,
    pub max_code: u32,
    pub _pad: [u32; 2],
}

/// How plane bytes reach the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UploadPath {
    /// `Queue::write_texture` from the frame's own memory: wgpu stages it
    /// in a buffer of its own per call. The default, and the baseline
    /// `bench_upload_5k7` measures.
    #[default]
    WriteTexture,
    /// A `wgpu::util::StagingBelt`: the rows copied into mapped chunks
    /// that are reused, then `copy_buffer_to_texture`. What the uploader
    /// switches to if the bench's trigger holds (DECISIONS D31).
    StagingBelt,
}

/// What one upload cost.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UploadStats {
    /// The CPU time of [`Uploader::upload`]: copies, recording and the
    /// submit.
    pub cpu_ms: f32,
    /// Plane bytes copied.
    pub bytes: u64,
    /// The GPU time of the latest upload whose timestamps are back (not
    /// necessarily this one's): ingest and mips. `None` without
    /// timestamps, or before any is back.
    pub gpu_ms: Option<f32>,
}

/// An integer texture a deep plane is uploaded into, with the ingest
/// pass's bind group for it.
#[derive(Debug)]
struct Scratch {
    format: wgpu::TextureFormat,
    size: (u32, u32),
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// The pipelines and buffers that fill slots. One per upload thread.
#[derive(Debug)]
pub struct Uploader {
    path: UploadPath,
    /// One per plane index, since a frame's planes are all written before
    /// its passes run.
    scratch: Vec<Option<Scratch>>,
    ingest_uniform: wgpu::Buffer,
    ingest_layout: wgpu::BindGroupLayout,
    ingest: HashMap<PlaneFormat, wgpu::RenderPipeline>,
    mips: HashMap<PlaneFormat, wgpu::RenderPipeline>,
    belt: Option<wgpu::util::StagingBelt>,
    timers: Option<GpuTimers>,
}

impl Uploader {
    pub fn new(gpu: &Gpu) -> Uploader {
        Self::with_path(gpu, UploadPath::default())
    }

    pub fn with_path(gpu: &Gpu, path: UploadPath) -> Uploader {
        let device = &gpu.device;
        let ingest_layout = layouts::ingest_input(device);
        let mip_layout = layouts::mip_input(device);
        let ingest_module = layouts::module(device, "ingest", layouts::INGEST_WGSL);
        let mips_module = layouts::module(device, "mips", layouts::MIPS_WGSL);
        let ingest = [PlaneFormat::R16F, PlaneFormat::Rg16F]
            .into_iter()
            .map(|f| {
                let p = layouts::fullscreen_pipeline(
                    device,
                    "ingest",
                    &ingest_module,
                    "fs_ingest",
                    &[],
                    &[&ingest_layout],
                    f.texture(),
                );
                (f, p)
            })
            .collect();
        let mips = PlaneFormat::ALL
            .into_iter()
            .map(|f| {
                let f16 = if f.is_deep() { 1.0 } else { 0.0 };
                let p = layouts::fullscreen_pipeline(
                    device,
                    "mips",
                    &mips_module,
                    "fs_mip",
                    &[("F16_TARGET", f16)],
                    &[&mip_layout],
                    f.texture(),
                );
                (f, p)
            })
            .collect();
        let ingest_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ingest uniform"),
            size: size_of::<IngestUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 64 MiB chunks: a 10-bit 8K plane pair fits one.
        let belt = (path == UploadPath::StagingBelt)
            .then(|| wgpu::util::StagingBelt::new(device.clone(), 64 << 20));
        Uploader {
            path,
            scratch: Vec::new(),
            ingest_uniform,
            ingest_layout,
            ingest,
            mips,
            belt,
            timers: GpuTimers::new(gpu, "upload"),
        }
    }

    pub fn path(&self) -> UploadPath {
        self.path
    }

    /// Uploads `frame`, a picture of the whole sphere, into `slot`.
    pub fn upload(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        self.upload_placed(gpu, frame, &Placement::FULL, slot)
    }

    /// Copies `frame`'s planes into `slot`, records the ingest and mip
    /// passes, and submits them once. Never waits on the GPU. `slot` must
    /// be one nothing draws from until the submission (slice 5's slots
    /// are reused only when no draw holds them; wgpu orders this
    /// submission after every earlier one that sampled the slot).
    pub fn upload_placed(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let start = Instant::now();
        let shape = SlotShape::of(frame.layout(), frame.size())?;
        if !slot.shape().fits(&shape) {
            return Err(RenderError::WrongSlot {
                got: shape.describe(),
                slot: slot.shape().describe(),
            });
        }
        // Every plane is checked before any is written, so a bad frame
        // leaves the slot as it was.
        let planes = shape
            .planes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let data = frame.plane(i).ok_or_else(|| {
                    RenderError::Invalid(format!("{} has no plane {i}", frame.layout().name))
                })?;
                let row = p.size.0 as usize * p.format.bytes_per_texel() as usize;
                check_plane(&data, p.size, row, i)?;
                Ok((data, row))
            })
            .collect::<Result<Vec<_>, RenderError>>()?;
        let device = &gpu.device;
        // Map callbacks: the belt's chunks and the timers' read-backs.
        let _ = device.poll(wgpu::PollType::Poll);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("upload"),
        });
        let mut bytes = 0u64;
        let deep = shape.planes.iter().any(|p| p.format.is_deep());
        if deep {
            let uniform = IngestUniform {
                shift: if shape.msb_aligned {
                    16 - u32::from(shape.bit_depth)
                } else {
                    0
                },
                max_code: (1u32 << shape.bit_depth) - 1,
                _pad: [0; 2],
            };
            gpu.queue
                .write_buffer(&self.ingest_uniform, 0, bytemuck::bytes_of(&uniform));
        }
        for (i, (p, (data, row))) in shape.planes.iter().zip(&planes).enumerate() {
            bytes += (row * p.size.1 as usize) as u64;
            if p.format.is_deep() {
                let scratch = self.scratch_for(gpu, i, p.format, p.size).texture.clone();
                self.write_plane(gpu, &mut enc, data, *row, &scratch, p.size)?;
                self.record_ingest(&mut enc, i, p.format, &slot.planes[i].levels[0]);
            } else {
                self.write_plane(gpu, &mut enc, data, *row, &slot.planes[i].texture, p.size)?;
            }
        }
        let span = self.timers.as_mut().and_then(|t| t.begin(&mut enc));
        self.record_mips(&mut enc, slot);
        if let (Some(t), Some(span)) = (self.timers.as_mut(), span) {
            t.end(&mut enc, span);
        }
        // The frame's own depth, with the slot's mip counts.
        slot.set_source(gpu, shape.bit_depth, frame.color(), placement);
        if let Some(belt) = self.belt.as_mut() {
            belt.finish_and_recall_on_submit(&enc);
        }
        gpu.queue.submit([enc.finish()]);
        Ok(UploadStats {
            cpu_ms: start.elapsed().as_secs_f32() * 1e3,
            bytes,
            gpu_ms: self.timers.as_mut().and_then(GpuTimers::poll),
        })
    }

    /// Runs map callbacks (the timers' read-backs, the belt's chunks)
    /// without waiting, and gives the latest GPU time read.
    pub fn poll(&mut self, gpu: &Gpu) -> Option<f32> {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        self.timers.as_mut().and_then(GpuTimers::poll)
    }

    /// Records the mip chain of every plane of `slot` from its level 0:
    /// what an upload ends with, and what a frame that filled level 0 on
    /// the GPU needs (see [`GpuFrame`]).
    pub fn record_mips(&self, enc: &mut wgpu::CommandEncoder, slot: &GpuFrame) {
        for (p, shape) in slot.planes.iter().zip(&slot.shape().planes) {
            let Some(pipeline) = self.mips.get(&shape.format) else {
                continue;
            };
            for (target, input) in p.levels[1..].iter().zip(&p.mip_inputs) {
                let mut pass = layouts::fullscreen_pass(enc, "mip", target);
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, input, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }

    fn record_ingest(
        &self,
        enc: &mut wgpu::CommandEncoder,
        plane: usize,
        format: PlaneFormat,
        target: &wgpu::TextureView,
    ) {
        let (Some(pipeline), Some(Some(scratch))) =
            (self.ingest.get(&format), self.scratch.get(plane))
        else {
            return;
        };
        let mut pass = layouts::fullscreen_pass(enc, "ingest", target);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &scratch.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The scratch texture for plane `i`, made or remade to fit.
    fn scratch_for(
        &mut self,
        gpu: &Gpu,
        i: usize,
        format: PlaneFormat,
        size: (u32, u32),
    ) -> &Scratch {
        let Some(want) = format.scratch() else {
            unreachable!("only deep planes have scratch textures");
        };
        if self.scratch.len() <= i {
            self.scratch.resize_with(i + 1, || None);
        }
        let fits = matches!(&self.scratch[i], Some(s) if s.format == want && s.size == size);
        if !fits {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ingest scratch"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: want,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ingest input"),
                layout: &self.ingest_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.ingest_uniform.as_entire_binding(),
                    },
                ],
            });
            self.scratch[i] = Some(Scratch {
                format: want,
                size,
                texture,
                bind_group,
            });
        }
        match &self.scratch[i] {
            Some(s) => s,
            None => unreachable!("made just above"),
        }
    }

    /// Plane bytes into level 0 of `texture`, by this uploader's path.
    fn write_plane(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        data: &PlaneData<'_>,
        row: usize,
        texture: &wgpu::Texture,
        size: (u32, u32),
    ) -> Result<(), RenderError> {
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let dest = wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        };
        match self.belt.as_mut() {
            None => {
                // Rows `stride` apart; the last may end without padding,
                // which wgpu accepts.
                let len = data.stride * (size.1 as usize - 1) + row;
                gpu.queue.write_texture(
                    dest,
                    &data.bytes[..len],
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(data.stride as u32),
                        rows_per_image: None,
                    },
                    extent,
                );
            }
            Some(belt) => {
                let pitch = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
                let total = (pitch * size.1 as usize).next_multiple_of(4) as u64;
                let Some(total) = wgpu::BufferSize::new(total) else {
                    return Err(RenderError::Invalid("an empty plane".into()));
                };
                let align = wgpu::BufferSize::new(256).unwrap_or(wgpu::BufferSize::MIN);
                let slice = belt.allocate(total, align);
                let offset = slice.offset();
                let buffer = slice.buffer().clone();
                let mut mapped = slice
                    .get_mapped_range_mut()
                    .map_err(|e| RenderError::Device(format!("mapping the staging belt: {e}")))?;
                if data.stride == pitch {
                    let n = pitch * (size.1 as usize - 1) + row;
                    mapped.slice(..n).copy_from_slice(&data.bytes[..n]);
                } else {
                    for y in 0..size.1 as usize {
                        let src = &data.bytes[y * data.stride..y * data.stride + row];
                        mapped
                            .slice(y * pitch..y * pitch + row)
                            .copy_from_slice(src);
                    }
                }
                drop(mapped);
                enc.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset,
                            bytes_per_row: Some(pitch as u32),
                            rows_per_image: None,
                        },
                    },
                    dest,
                    extent,
                );
            }
        }
        Ok(())
    }
}

/// `Invalid` unless plane `i` holds `size.1` rows of `row` bytes.
fn check_plane(
    data: &PlaneData<'_>,
    size: (u32, u32),
    row: usize,
    i: usize,
) -> Result<(), RenderError> {
    let need = data
        .stride
        .checked_mul(size.1 as usize - 1)
        .and_then(|n| n.checked_add(row));
    if (data.width, data.height) != size
        || data.stride < row
        || need.is_none_or(|n| n > data.bytes.len())
    {
        return Err(RenderError::Invalid(format!(
            "plane {i} is {}x{} in {} bytes with rows {} apart, where {}x{} of {row}-byte rows are wanted",
            data.width,
            data.height,
            data.bytes.len(),
            data.stride,
            size.0,
            size.1
        )));
    }
    if u32::try_from(data.stride).is_err() {
        return Err(RenderError::Invalid(format!(
            "plane {i}'s rows are too far apart"
        )));
    }
    Ok(())
}
