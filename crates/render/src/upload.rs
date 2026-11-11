//! Frames into slots (M1 plan section 4.4): plane bytes into level 0,
//! deep planes through the ingest pass, then the mip chain, in one
//! submission per frame that never waits on the GPU. Planes already on
//! the GPU (slice 5b's DMA-BUF imports) take the same way, copied there,
//! and so do planes a copier put in a GPU buffer (path B). An 8-bit
//! plane's mips are a compute pass a level into a buffer and a copy, so
//! that nothing renders to it (DECISIONS D77); a deep plane's are a
//! render pass a level.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use spherecut_core::video::{PlaneData, VideoPlanes};

use crate::color::Placement;
use crate::error::RenderError;
use crate::frame::{GpuFrame, PlaneTexture};
use crate::gpu::Gpu;
use crate::layouts;
use crate::plane::{PlaneFormat, PlaneShape, SlotShape};
use crate::timers::GpuTimers;
use crate::vkshare::{BufferPlane, PlaneRegion};

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
    /// necessarily this one's): its ingest passes and mips, on either
    /// path (the belt's copies come before the span, as `write_texture`'s
    /// do). `None` without timestamps, or before any is back. Each
    /// reading is given once, here or by a later [`Uploader::poll`],
    /// never both: another thread's poll of the device can bring this
    /// upload's own back before the upload returns.
    pub gpu_ms: Option<f32>,
}

/// How [`Uploader::record_mips`] makes an 8-bit plane's levels. Only
/// tests ask for anything but `Compute`.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EightBitMips {
    /// A compute pass a level into a buffer, then a copy into the level
    /// (DECISIONS D77).
    #[default]
    Compute,
    /// A render pass a level, as before D77, into a slot made by
    /// [`GpuFrame::renderable`]: the reference the compute pass is held
    /// to, and the old layout the upload bench's test of a step's own
    /// blocks runs on.
    RenderPass,
    /// The compute pass with each mean truncated, one code off wherever it
    /// rounds up: the control that holding it to the reference fails.
    Truncated,
}

/// The work-group size of `shaders/mips8.wgsl`'s `cs_mip8`: words of a
/// row across, rows down.
const MIP8_GROUP: (u32, u32) = (64, 4);

/// An integer texture a deep plane is uploaded into, with the ingest
/// pass's bind group for it.
#[derive(Debug)]
struct Scratch {
    format: wgpu::TextureFormat,
    size: (u32, u32),
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// The buffer the 8-bit mip pass writes a plane's levels into, one region
/// a level from level 1, each copied into its level once written.
#[derive(Debug)]
struct MipScratch {
    format: PlaneFormat,
    size: (u32, u32),
    buffer: wgpu::Buffer,
    regions: Vec<MipRegion>,
}

/// Where one level sits in a [`MipScratch`]: its rows, each padded to
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, and the pass's bind group for exactly
/// them, from which the shader takes the row pitch.
#[derive(Debug)]
struct MipRegion {
    offset: u64,
    bytes_per_row: u32,
    size: (u32, u32),
    output: wgpu::BindGroup,
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
    /// The render pass a level: deep formats, and 8-bit ones only under
    /// [`EightBitMips::RenderPass`].
    mips: HashMap<PlaneFormat, wgpu::RenderPipeline>,
    /// The compute pass a level, for 8-bit formats.
    mips8: HashMap<PlaneFormat, wgpu::ComputePipeline>,
    mip_output: wgpu::BindGroupLayout,
    /// For the buffers of `mip_scratch`.
    device: wgpu::Device,
    /// One per plane index, made or remade to fit when the mips are
    /// recorded, which takes `&self`.
    mip_scratch: Mutex<Vec<Option<MipScratch>>>,
    eight_bit: EightBitMips,
    belt: Option<wgpu::util::StagingBelt>,
    timers: Option<GpuTimers>,
}

impl Uploader {
    pub fn new(gpu: &Gpu) -> Uploader {
        Self::with_path(gpu, UploadPath::default())
    }

    pub fn with_path(gpu: &Gpu, path: UploadPath) -> Uploader {
        Self::with_eight_bit_mips(gpu, path, EightBitMips::default())
    }

    /// An uploader on `path` whose 8-bit mips are made as `eight_bit`
    /// says, for tests only: the one that holds the compute pass to the
    /// render passes it replaced, and the upload bench's test of a step's
    /// own blocks.
    #[doc(hidden)]
    pub fn with_eight_bit_mips(gpu: &Gpu, path: UploadPath, eight_bit: EightBitMips) -> Uploader {
        let device = &gpu.device;
        let ingest_layout = layouts::ingest_input(device);
        let mip_layout = layouts::mip_input(device);
        let mip_output = layouts::mip_output(device);
        let ingest_module = layouts::module(device, "ingest", layouts::INGEST_WGSL);
        let mips_module = layouts::module(device, "mips", layouts::MIPS_WGSL);
        let mips8_module = layouts::module(device, "mips8", layouts::MIPS8_WGSL);
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
            .filter(|f| f.is_deep() || eight_bit == EightBitMips::RenderPass)
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
        let mips8_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mips8"),
            bind_group_layouts: &[Some(&mip_layout), Some(&mip_output)],
            immediate_size: 0,
        });
        let truncate = if eight_bit == EightBitMips::Truncated {
            1.0
        } else {
            0.0
        };
        let mips8 = [PlaneFormat::R8, PlaneFormat::Rg8]
            .into_iter()
            .map(|f| {
                let constants = [
                    ("COMPONENTS", f64::from(f.components())),
                    ("TRUNCATE", truncate),
                ];
                let p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("mips8"),
                    layout: Some(&mips8_layout),
                    module: &mips8_module,
                    entry_point: Some("cs_mip8"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &constants,
                        ..Default::default()
                    },
                    cache: None,
                });
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
            mips8,
            mip_output,
            device: device.clone(),
            mip_scratch: Mutex::new(Vec::new()),
            eight_bit,
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
    /// passes, and submits them once. Never waits on the GPU. Of a stereo
    /// frame only the shown eye is copied, so `slot` is made for
    /// [`Placement::shown`]'s size. `slot` must be one nothing draws from
    /// until the submission (slice 5's slots are reused only when no draw
    /// holds them; wgpu orders this submission after every earlier one
    /// that sampled the slot).
    pub fn upload_placed(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let start = Instant::now();
        let (whole, shape) = shapes(gpu, frame, placement, slot)?;
        // Every plane is checked, whole, before any is written, so a bad
        // frame leaves the slot as it was. What is copied is the shown
        // part's rows, from the top-left corner.
        let planes = shape
            .planes
            .iter()
            .zip(&whole.planes)
            .enumerate()
            .map(|(i, (p, w))| {
                let data = frame.plane(i).ok_or_else(|| {
                    RenderError::Invalid(format!("{} has no plane {i}", frame.layout().name))
                })?;
                let bpt = p.format.bytes_per_texel() as usize;
                check_plane(&data, w.size, w.size.0 as usize * bpt, i)?;
                Ok((data, p.size.0 as usize * bpt))
            })
            .collect::<Result<Vec<_>, RenderError>>()?;
        let device = &gpu.device;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("upload"),
        });
        let mut bytes = 0u64;
        self.write_ingest_uniform(gpu, &shape);
        // The planes first: deep ones into their scratch textures. Then
        // the GPU's own work, timed: the ingest passes and the mips.
        for (i, (p, (data, row))) in shape.planes.iter().zip(&planes).enumerate() {
            bytes += (row * p.size.1 as usize) as u64;
            let texture = if p.format.is_deep() {
                self.scratch_for(gpu, i, p.format, p.size).texture.clone()
            } else {
                slot.planes[i].texture.clone()
            };
            self.write_plane(gpu, &mut enc, data, *row, &texture, p.size)?;
        }
        let span = self.timers.as_mut().and_then(|t| t.begin(&mut enc));
        for (i, p) in shape.planes.iter().enumerate() {
            if let Some(Some(scratch)) = self.scratch.get(i).filter(|_| p.format.is_deep()) {
                let input = &scratch.bind_group;
                self.record_ingest(&mut enc, p.format, input, &slot.planes[i].levels[0]);
            }
        }
        self.record_mips(&mut enc, slot);
        if let (Some(t), Some(span)) = (self.timers.as_mut(), span) {
            t.end(&mut enc, span);
        }
        // The frame's own depth, with the slot's mip counts.
        slot.set_source(gpu, shape.bit_depth, frame.color(), placement);
        if let Some(belt) = self.belt.as_mut() {
            belt.finish_and_recall_on_submit(&enc);
        }
        // Not while the UI presents (D41).
        gpu.submit_gate().wait();
        gpu.queue.submit([enc.finish()]);
        Ok(UploadStats {
            cpu_ms: start.elapsed().as_secs_f32() * 1e3,
            bytes,
            gpu_ms: self.timers.as_mut().and_then(GpuTimers::poll),
        })
    }

    /// Fills `slot` from planes already on the GPU, one texture a plane in
    /// [`dmabuf::imported_format`](crate::dmabuf::imported_format) at the
    /// frame's whole plane sizes, with [`dmabuf::USAGE`](crate::dmabuf::USAGE):
    /// a DMA-BUF import, or any texture made so. An 8-bit plane's shown
    /// part is copied into level 0, a deep one goes through the ingest
    /// pass; then the mips, all in one submission, as
    /// [`upload_placed`](Self::upload_placed) does from bytes. `frame`
    /// gives the layout, size and colour; its bytes aren't read. The GPU
    /// time covers the copies too, since they replace the CPU's. Never
    /// waits on the GPU; the planes must stay as they are until it is
    /// done (`Queue::on_submitted_work_done`).
    pub fn upload_imported(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        planes: &[wgpu::Texture],
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let start = Instant::now();
        let (whole, shape) = shapes(gpu, frame, placement, slot)?;
        if planes.len() != whole.planes.len() {
            return Err(RenderError::Invalid(format!(
                "{} textures for {}'s {} planes",
                planes.len(),
                frame.layout().name,
                whole.planes.len()
            )));
        }
        for (i, (t, w)) in planes.iter().zip(&whole.planes).enumerate() {
            let want = crate::dmabuf::imported_format(w.format);
            let size = (t.width(), t.height());
            if t.format() != want
                || size != w.size
                || t.mip_level_count() != 1
                || t.sample_count() != 1
                || t.depth_or_array_layers() != 1
                || !t.usage().contains(crate::dmabuf::USAGE)
            {
                return Err(RenderError::Invalid(format!(
                    "plane {i}'s texture is {:?} {}x{}, where {want:?} {}x{} with {:?} is wanted",
                    t.format(),
                    size.0,
                    size.1,
                    w.size.0,
                    w.size.1,
                    crate::dmabuf::USAGE
                )));
            }
        }
        let device = &gpu.device;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("upload imported"),
        });
        self.write_ingest_uniform(gpu, &shape);
        let span = self.timers.as_mut().and_then(|t| t.begin(&mut enc));
        for (i, (p, t)) in shape.planes.iter().zip(planes).enumerate() {
            let target = &slot.planes[i];
            if p.format.is_deep() {
                let view = t.create_view(&wgpu::TextureViewDescriptor::default());
                let input = self.ingest_bind_group(gpu, &view);
                self.record_ingest(&mut enc, p.format, &input, &target.levels[0]);
            } else {
                enc.copy_texture_to_texture(
                    t.as_image_copy(),
                    target.texture.as_image_copy(),
                    wgpu::Extent3d {
                        width: p.size.0,
                        height: p.size.1,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        self.record_mips(&mut enc, slot);
        if let (Some(t), Some(span)) = (self.timers.as_mut(), span) {
            t.end(&mut enc, span);
        }
        slot.set_source(gpu, shape.bit_depth, frame.color(), placement);
        gpu.submit_gate().wait();
        gpu.queue.submit([enc.finish()]);
        Ok(UploadStats {
            cpu_ms: start.elapsed().as_secs_f32() * 1e3,
            bytes: 0,
            gpu_ms: self.timers.as_mut().and_then(GpuTimers::poll),
        })
    }

    /// Fills `slot` from planes already in a GPU buffer, laid out as
    /// `planes` say (slice 5b's path B: a [`Copier`](crate::vkshare::Copier)
    /// copied the decoder's picture there, each plane the shown part, from
    /// the top-left corner). The rest is the copy path's: an 8-bit plane
    /// into level 0, a deep one into its scratch texture and the ingest
    /// pass, then the mips, in one submission. `frame` gives the layout,
    /// size and colour; its bytes aren't read. The GPU time covers the
    /// copies too. Never waits on the GPU; `buffer` must stay as it is
    /// until it is done (`Queue::on_submitted_work_done`).
    pub fn upload_from_buffer(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        buffer: &wgpu::Buffer,
        planes: &[BufferPlane],
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let start = Instant::now();
        let (_, shape) = shapes(gpu, frame, placement, slot)?;
        if planes.len() != shape.planes.len() {
            return Err(RenderError::Invalid(format!(
                "{} buffer planes for {}'s {} planes",
                planes.len(),
                frame.layout().name,
                shape.planes.len()
            )));
        }
        for (i, (b, p)) in planes.iter().zip(&shape.planes).enumerate() {
            let row = u64::from(p.size.0) * u64::from(p.format.bytes_per_texel());
            let end = b.offset + u64::from(b.bytes_per_row) * u64::from(p.size.1);
            if b.size != p.size
                || b.bytes_per_texel != p.format.bytes_per_texel()
                || u64::from(b.bytes_per_row) < row
                || b.bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT != 0
                || end > buffer.size()
            {
                return Err(RenderError::Invalid(format!(
                    "buffer plane {i} is {}x{} at {} with {}-byte rows in {} bytes, for a \
                     {}x{} plane of {row}-byte rows",
                    b.size.0,
                    b.size.1,
                    b.offset,
                    b.bytes_per_row,
                    buffer.size(),
                    p.size.0,
                    p.size.1
                )));
            }
        }
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("upload from buffer"),
            });
        self.write_ingest_uniform(gpu, &shape);
        let span = self.timers.as_mut().and_then(|t| t.begin(&mut enc));
        for (i, (p, b)) in shape.planes.iter().zip(planes).enumerate() {
            let texture = if p.format.is_deep() {
                self.scratch_for(gpu, i, p.format, p.size).texture.clone()
            } else {
                slot.planes[i].texture.clone()
            };
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: b.offset,
                        bytes_per_row: Some(b.bytes_per_row),
                        rows_per_image: Some(p.size.1),
                    },
                },
                texture.as_image_copy(),
                wgpu::Extent3d {
                    width: p.size.0,
                    height: p.size.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        for (i, p) in shape.planes.iter().enumerate() {
            if let Some(Some(scratch)) = self.scratch.get(i).filter(|_| p.format.is_deep()) {
                let input = &scratch.bind_group;
                self.record_ingest(&mut enc, p.format, input, &slot.planes[i].levels[0]);
            }
        }
        self.record_mips(&mut enc, slot);
        if let (Some(t), Some(span)) = (self.timers.as_mut(), span) {
            t.end(&mut enc, span);
        }
        slot.set_source(gpu, shape.bit_depth, frame.color(), placement);
        gpu.submit_gate().wait();
        gpu.queue.submit([enc.finish()]);
        Ok(UploadStats {
            cpu_ms: start.elapsed().as_secs_f32() * 1e3,
            bytes: 0,
            gpu_ms: self.timers.as_mut().and_then(GpuTimers::poll),
        })
    }

    /// Runs map callbacks (the timers' read-backs, the belt's chunks)
    /// without waiting, and gives the GPU time read since the last call,
    /// if any. On a lost device it reads nothing more. A reading an
    /// upload's [`UploadStats::gpu_ms`] gave is not given again here.
    pub fn poll(&mut self, gpu: &Gpu) -> Option<f32> {
        if gpu.is_lost() {
            return None;
        }
        self.timers.as_mut().and_then(GpuTimers::poll)
    }

    /// Records the mip chain of every plane of `slot` from its level 0:
    /// what an upload ends with, and what a frame that filled level 0 on
    /// the GPU needs (see [`GpuFrame`]). A deep plane's levels are a render
    /// pass each; an 8-bit plane's a compute pass each, into a buffer of
    /// this uploader's, and a copy from it into the level (D77).
    pub fn record_mips(&self, enc: &mut wgpu::CommandEncoder, slot: &GpuFrame) {
        for (i, (p, shape)) in slot.planes.iter().zip(&slot.shape().planes).enumerate() {
            if !shape.format.is_deep() && self.eight_bit != EightBitMips::RenderPass {
                self.record_mips8(enc, i, p, shape);
                continue;
            }
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

    /// Levels 1 and on of 8-bit plane `i`, `plane`, each from the one
    /// above: the compute pass into the level's region of the plane's
    /// [`MipScratch`], then the region copied into the level, which the
    /// next level's pass reads.
    fn record_mips8(
        &self,
        enc: &mut wgpu::CommandEncoder,
        i: usize,
        plane: &PlaneTexture,
        shape: &PlaneShape,
    ) {
        if shape.levels < 2 {
            return;
        }
        let Some(pipeline) = self.mips8.get(&shape.format) else {
            return;
        };
        let mut all = self
            .mip_scratch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if all.len() <= i {
            all.resize_with(i + 1, || None);
        }
        let fits = matches!(&all[i], Some(s) if s.format == shape.format
            && s.size == shape.size
            && s.regions.len() + 1 >= shape.levels as usize);
        if !fits {
            all[i] = Some(self.mip_scratch(shape));
        }
        let Some(scratch) = &all[i] else {
            unreachable!("made just above");
        };
        for (l, (region, input)) in scratch.regions.iter().zip(&plane.mip_inputs).enumerate() {
            let (w, h) = region.size;
            let words = (w * shape.format.components()).div_ceil(4);
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("mip"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, input, &[]);
                pass.set_bind_group(1, &region.output, &[]);
                pass.dispatch_workgroups(words.div_ceil(MIP8_GROUP.0), h.div_ceil(MIP8_GROUP.1), 1);
            }
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &scratch.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: region.offset,
                        bytes_per_row: Some(region.bytes_per_row),
                        rows_per_image: Some(h),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &plane.texture,
                    mip_level: l as u32 + 1,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// A [`MipScratch`] for an 8-bit plane of `shape`: a region for each
    /// of its levels from 1, one after another, every offset a multiple
    /// of 256 bytes, as both a copy and a storage binding take it.
    fn mip_scratch(&self, shape: &PlaneShape) -> MipScratch {
        let device = &self.device;
        let bpt = shape.format.bytes_per_texel();
        let mut offset = 0u64;
        let mut layout = Vec::new();
        for l in 1..shape.levels {
            let size = ((shape.size.0 >> l).max(1), (shape.size.1 >> l).max(1));
            let bytes_per_row = (size.0 * bpt).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            layout.push((offset, bytes_per_row, size));
            offset += u64::from(bytes_per_row) * u64::from(size.1);
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mip scratch"),
            size: offset.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let regions = layout
            .into_iter()
            .map(|(offset, bytes_per_row, size)| {
                let bytes = u64::from(bytes_per_row) * u64::from(size.1);
                let output = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mip output"),
                    layout: &self.mip_output,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset,
                            size: wgpu::BufferSize::new(bytes),
                        }),
                    }],
                });
                MipRegion {
                    offset,
                    bytes_per_row,
                    size,
                    output,
                }
            })
            .collect();
        MipScratch {
            format: shape.format,
            size: shape.size,
            buffer,
            regions,
        }
    }

    /// The ingest pass from `input` (integers of a deep plane, and the
    /// uniform) into `target`, a level 0 of `format`.
    fn record_ingest(
        &self,
        enc: &mut wgpu::CommandEncoder,
        format: PlaneFormat,
        input: &wgpu::BindGroup,
        target: &wgpu::TextureView,
    ) {
        let Some(pipeline) = self.ingest.get(&format) else {
            return;
        };
        let mut pass = layouts::fullscreen_pass(enc, "ingest", target);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, input, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The ingest uniform for `shape`'s depth and alignment, when it has a
    /// deep plane.
    fn write_ingest_uniform(&self, gpu: &Gpu, shape: &SlotShape) {
        if !shape.planes.iter().any(|p| p.format.is_deep()) {
            return;
        }
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

    /// The ingest pass's input: `view` of a deep plane's integers, and the
    /// uniform.
    fn ingest_bind_group(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ingest input"),
            layout: &self.ingest_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.ingest_uniform.as_entire_binding(),
                },
            ],
        })
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
            let bind_group = self.ingest_bind_group(gpu, &view);
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

/// `frame`'s shape whole and as `placement` shows it, after checking the
/// device (which polls it too, running map callbacks: the belt's chunks
/// and the timers' read-backs) and that the shown part fits `slot`.
/// What a [`Copier`](crate::vkshare::Copier) copies of `frame` for
/// `placement`, plane by plane: the shown part from the top-left corner,
/// as [`Uploader::upload_from_buffer`] takes it.
pub fn shown_regions(
    frame: &dyn VideoPlanes,
    placement: &Placement,
) -> Result<Vec<PlaneRegion>, RenderError> {
    let shape = SlotShape::of(frame.layout(), placement.shown(frame.size()))?;
    Ok(shape
        .planes
        .iter()
        .map(|p| PlaneRegion {
            bytes_per_texel: p.format.bytes_per_texel(),
            size: p.size,
        })
        .collect())
}

fn shapes(
    gpu: &Gpu,
    frame: &dyn VideoPlanes,
    placement: &Placement,
    slot: &GpuFrame,
) -> Result<(SlotShape, SlotShape), RenderError> {
    gpu.check()?;
    let whole = SlotShape::of(frame.layout(), frame.size())?;
    let shape = SlotShape::of(frame.layout(), placement.shown(frame.size()))?;
    if !slot.shape().fits(&shape) {
        return Err(RenderError::WrongSlot {
            got: shape.describe(),
            slot: slot.shape().describe(),
        });
    }
    Ok((whole, shape))
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
