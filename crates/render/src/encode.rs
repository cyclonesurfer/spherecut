//! The encode pass (M1 plan section 4.3): a linear target into display
//! values with the sRGB OETF, one texel per pixel.

use crate::gpu::Gpu;
use crate::layouts;
use crate::project::LinearTarget;

/// The encode shader's `Place` uniform (`shaders/encode.wgsl`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PlaceUniform {
    pub origin: [i32; 2],
    pub extent: [i32; 2],
}

/// The encode pipeline for one output format: egui's `Rgba8Unorm` or
/// `Bgra8Unorm` (slice 7), or the offscreen `Rgba8Unorm` of
/// [`crate::render_still`]. The format must not be an sRGB one, since the
/// shader applies the OETF itself.
#[derive(Debug)]
pub struct Encoder {
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    queue: wgpu::Queue,
}

impl Encoder {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Encoder {
        let device = &gpu.device;
        let module = layouts::module(device, "encode", layouts::ENCODE_WGSL);
        let pipeline = layouts::fullscreen_pipeline(
            device,
            "encode",
            &module,
            "fs_encode",
            &[],
            &[&layouts::encode_input(device)],
            format,
        );
        Encoder {
            format,
            pipeline,
            queue: gpu.queue.clone(),
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Draws `target`'s extent into `pass` with its texel (0, 0) at
    /// `origin_px` of the pass's attachment. The pass's viewport and
    /// scissor bound what is drawn; pixels outside the target's extent
    /// are black. The origin is written through the queue, so it
    /// takes effect at the next submit: one draw per target per
    /// submission.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        target: &LinearTarget,
        origin_px: [u32; 2],
    ) {
        let int = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let (w, h) = target.extent();
        let place = PlaceUniform {
            origin: origin_px.map(int),
            extent: [int(w), int(h)],
        };
        self.queue
            .write_buffer(&target.place, 0, bytemuck::bytes_of(&place));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &target.encode_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
