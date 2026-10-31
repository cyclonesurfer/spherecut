//! Motion blur (M2 plan section 7.4, amendment 2 items 17 and 18): a
//! frame is the mean, in linear light, of `n` views of one source
//! picture spread over the shutter, `n` from how far the view moves in
//! output pixels. [`Blur`] sums the views' projections in a storage
//! buffer of `vec4<f32>` and writes the mean back into the linear target.

use glam::DVec2;
use wgpu::util::DeviceExt;

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::layouts;
use crate::project::{LinearTarget, ViewParams};

/// The most views a frame averages (amendment 2 item 17); a frame that
/// would need more is counted as capped.
pub const MAX_SAMPLES: u32 = 64;
/// How far, in output pixels, the view may move between two views.
pub const PIXELS_PER_SAMPLE: f64 = 1.5;

/// How many views a frame whose view moves `s` output pixels over the
/// shutter averages, `clamp(⌈s / 1.5⌉, 1, 64)`, and whether it hit the
/// cap.
pub fn sample_count(s: f64) -> (u32, bool) {
    let want = (s / PIXELS_PER_SAMPLE).ceil();
    if want.is_nan() || want > f64::from(MAX_SAMPLES) {
        return (MAX_SAMPLES, true);
    }
    (want.max(1.0) as u32, false)
}

/// The view's times over a shutter `open` long, centred on `t`, for `n`
/// views: the middles of `n` equal parts.
pub fn sample_times(t: f64, open: f64, n: u32) -> Vec<f64> {
    (0..n)
        .map(|k| t - open / 2.0 + (f64::from(k) + 0.5) * open / f64::from(n))
        .collect()
}

/// How far, in output pixels, the frame moves from view `a` to view
/// `b`: the largest move of its centre, corners and edge midpoints
/// (amendment 2 item 17, so that a roll or a zoom counts), each the
/// distance from where `a` shows a direction to where `b` shows it.
/// Points that either view can't show are left out; equirect output
/// measures across its seam the short way.
pub fn displacement(a: &ViewParams, b: &ViewParams, size: (u32, u32)) -> f64 {
    let [fx, fy, fw, fh] = a.frame(size);
    let mut worst = 0f64;
    for i in 0..3 {
        for j in 0..3 {
            let p = DVec2::new(fx + fw * f64::from(i) / 2.0, fy + fh * f64::from(j) / 2.0);
            if a.theta(size, p.x, p.y) > std::f64::consts::PI {
                continue;
            }
            let Some(q) = b.pixel_of(size, a.dir(size, p.x, p.y)) else {
                continue;
            };
            let mut d = q - p;
            if a.out == crate::project::OutKind::Equirect {
                let w = f64::from(size.0);
                d.x = d.x.rem_euclid(w);
                d.x = d.x.min(w - d.x);
            }
            worst = worst.max(d.length());
        }
    }
    worst
}

/// The `Sum` uniform of `shaders/blur.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SumUniform {
    pub size: [u32; 2],
    pub n: f32,
    pub _pad: f32,
}

/// The blur's two passes and its sum, for one target size at a time.
#[derive(Debug)]
pub struct Blur {
    add_layout: wgpu::BindGroupLayout,
    resolve_layout: wgpu::BindGroupLayout,
    add: wgpu::ComputePipeline,
    resolve: wgpu::RenderPipeline,
    /// The sum and the size it is for.
    acc: Option<(wgpu::Buffer, (u32, u32))>,
}

fn entry(
    binding: u32,
    stage: wgpu::ShaderStages,
    ty: wgpu::BindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: stage,
        ty,
        count: None,
    }
}

fn uniform() -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: wgpu::BufferSize::new(size_of::<SumUniform>() as u64),
    }
}

fn storage(read_only: bool) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Storage { read_only },
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

impl Blur {
    pub fn new(gpu: &Gpu) -> Blur {
        let device = &gpu.device;
        let compute = wgpu::ShaderStages::COMPUTE;
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let add_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur add"),
            entries: &[
                entry(
                    0,
                    compute,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(1, compute, uniform()),
                entry(2, compute, storage(false)),
            ],
        });
        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur resolve"),
            entries: &[
                entry(1, fragment, uniform()),
                entry(3, fragment, storage(true)),
            ],
        });
        let module = layouts::module(device, "blur", layouts::BLUR_WGSL);
        let add_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur add"),
            bind_group_layouts: &[Some(&add_layout)],
            immediate_size: 0,
        });
        let add = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blur add"),
            layout: Some(&add_pipeline_layout),
            module: &module,
            entry_point: Some("cs_add"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let resolve = layouts::fullscreen_pipeline(
            device,
            "blur resolve",
            &module,
            "fs_resolve",
            &[],
            &[&resolve_layout],
            LinearTarget::FORMAT,
        );
        Blur {
            add_layout,
            resolve_layout,
            add,
            resolve,
            acc: None,
        }
    }

    fn sum_uniform(gpu: &Gpu, size: (u32, u32), n: u32) -> wgpu::Buffer {
        let u = SumUniform {
            size: [size.0, size.1],
            n: n as f32,
            _pad: 0.0,
        };
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur sum"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            })
    }

    /// Starts a frame of `size`: the sum is cleared, and made when the
    /// size changed.
    pub fn begin(&mut self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, size: (u32, u32)) {
        let fits = self.acc.as_ref().is_some_and(|(_, s)| *s == size);
        if !fits {
            let bytes = u64::from(size.0) * u64::from(size.1) * 16;
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blur sum"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.acc = Some((buffer, size));
        }
        if let Some((buffer, _)) = &self.acc {
            enc.clear_buffer(buffer, 0, None);
        }
    }

    fn sum(&self, target: &LinearTarget) -> Result<&wgpu::Buffer, RenderError> {
        match &self.acc {
            Some((b, s)) if *s == target.extent() => Ok(b),
            _ => Err(RenderError::Invalid(format!(
                "the blur's sum isn't begun for a {:?} target",
                target.extent()
            ))),
        }
    }

    /// Adds `target`'s drawn part to the sum. The projection into it
    /// must come first in the same encoder.
    pub fn add(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        target: &LinearTarget,
    ) -> Result<(), RenderError> {
        let acc = self.sum(target)?;
        let size = target.extent();
        let u = Self::sum_uniform(gpu, size, 1);
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur add"),
            layout: &self.add_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(target.texture_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: u.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: acc.as_entire_binding(),
                },
            ],
        });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("blur add"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.add);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        Ok(())
    }

    /// Writes the mean of the `n` views summed into `target`'s drawn part,
    /// each value its nearest half.
    pub fn resolve(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        target: &LinearTarget,
        n: u32,
    ) -> Result<(), RenderError> {
        let acc = self.sum(target)?;
        let size = target.extent();
        let u = Self::sum_uniform(gpu, size, n.max(1));
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur resolve"),
            layout: &self.resolve_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: u.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: acc.as_entire_binding(),
                },
            ],
        });
        let mut pass = layouts::fullscreen_pass(enc, "blur resolve", target.texture_view());
        pass.set_viewport(0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0);
        pass.set_pipeline(&self.resolve);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DQuat;
    use spherecut_sphere::perspective::{Lens, Projection};

    #[test]
    fn the_count_follows_the_move_and_stops_at_the_cap() {
        assert_eq!(sample_count(0.0), (1, false));
        assert_eq!(sample_count(1.5), (1, false));
        assert_eq!(sample_count(1.6), (2, false));
        assert_eq!(sample_count(96.0), (64, false));
        assert_eq!(sample_count(96.1), (64, true));
        assert_eq!(sample_count(f64::INFINITY), (64, true));
        assert_eq!(sample_count(f64::NAN), (64, true));
    }

    #[test]
    fn the_times_fill_the_shutter_evenly() {
        let t = sample_times(1.0, 0.5, 4);
        assert_eq!(t, [0.8125, 0.9375, 1.0625, 1.1875]);
        assert_eq!(sample_times(2.0, 0.1, 1), [2.0]);
    }

    #[test]
    fn a_pan_a_roll_and_a_zoom_all_move_the_frame() {
        let size = (256, 144);
        let lens = Lens::new(Projection::Rectilinear, 90f64.to_radians());
        let still = ViewParams::new(DQuat::IDENTITY, lens);
        assert!(displacement(&still, &still, size) < 1e-9);
        // A pan moves the sides more than the middle, f·(tan 51° − tan
        // 45°) at the right side's midpoint, 128 px out, and the corners
        // a little more again.
        let pan = ViewParams::new(DQuat::from_rotation_y(-6f64.to_radians()), lens);
        let s = displacement(&still, &pan, size);
        let edge = 128.0 * (51f64.to_radians().tan() - 1.0);
        assert!(s >= edge && s < edge * 1.1, "{s} against {edge}");
        // A roll leaves the middle where it is and moves the corners.
        let roll = ViewParams::new(DQuat::from_rotation_z(2f64.to_radians()), lens);
        assert!(displacement(&still, &roll, size) > 4.0);
        // So does a zoom.
        let zoom = ViewParams::new(
            DQuat::IDENTITY,
            Lens::new(Projection::Rectilinear, 80f64.to_radians()),
        );
        assert!(displacement(&still, &zoom, size) > 10.0);
        // Equirect output turned 10° moves a ninth of 90° of its width,
        // across the seam the short way.
        let e = ViewParams::equirect(DQuat::IDENTITY);
        let turned = ViewParams::equirect(DQuat::from_rotation_y(-10f64.to_radians()));
        let s = displacement(&e, &turned, (360, 180));
        assert!((s - 10.0).abs() < 1e-6, "{s}");
    }
}
