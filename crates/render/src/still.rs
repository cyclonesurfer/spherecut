//! One frame, headless (M1 plan section 4.5): upload, project, encode and
//! read back. `spherecut frame` and the goldens use it.

use spherecut_core::video::VideoPlanes;

use crate::color::Placement;
use crate::encode::Encoder;
use crate::error::RenderError;
use crate::frame::GpuFrame;
use crate::gpu::Gpu;
use crate::plane::SlotShape;
use crate::project::{DebugMode, Filter, LinearTarget, Projector, ViewParams};
use crate::readback::{self, Texels};
use crate::upload::Uploader;

/// What a still is read back as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Depth {
    /// The encode pass into `Rgba8Unorm`, rounded to 8-bit codes in the
    /// shader: what the viewer shows.
    #[default]
    Eight,
    /// The linear target with the sRGB OETF applied on the CPU in f32,
    /// as 16-bit codes: `frame --depth 16`.
    Sixteen,
    /// The linear target as it is, for checks of the LOD heat map and of
    /// linear light.
    Linear,
}

/// How [`render_still`] renders.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StillOptions {
    pub filter: Filter,
    pub debug: DebugMode,
    pub depth: Depth,
    pub placement: Placement,
}

/// A rendered picture's pixels: R, G, B per pixel, rows top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub enum Pixels {
    Rgb8(Vec<u8>),
    Rgb16(Vec<u16>),
    /// Linear values ([`Depth::Linear`]).
    LinearF32(Vec<f32>),
}

/// A rendered picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

impl Image {
    /// Every channel of every pixel in `[0, 1]` (codes over their
    /// maximum), or as it is for linear pixels: for comparisons.
    pub fn unit_rgb(&self) -> Vec<f64> {
        match &self.pixels {
            Pixels::Rgb8(p) => p.iter().map(|&v| f64::from(v) / 255.0).collect(),
            Pixels::Rgb16(p) => p.iter().map(|&v| f64::from(v) / 65535.0).collect(),
            Pixels::LinearF32(p) => p.iter().map(|&v| f64::from(v)).collect(),
        }
    }

    /// Pixel `(x, y)`'s channels as [`Image::unit_rgb`] gives them.
    pub fn unit_at(&self, x: u32, y: u32) -> Option<[f64; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = 3 * (y as usize * self.width as usize + x as usize);
        let get = |c: usize| match &self.pixels {
            Pixels::Rgb8(p) => f64::from(p[i + c]) / 255.0,
            Pixels::Rgb16(p) => f64::from(p[i + c]) / 65535.0,
            Pixels::LinearF32(p) => f64::from(p[i + c]),
        };
        Some([get(0), get(1), get(2)])
    }

    /// The picture as a PNG, RGB at 8 or 16 bits. `Invalid` for linear
    /// pixels, which a PNG doesn't hold.
    pub fn to_png(&self) -> Result<Vec<u8>, RenderError> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut out);
        let result = match &self.pixels {
            Pixels::Rgb8(p) => {
                encoder.write_image(p, self.width, self.height, image::ExtendedColorType::Rgb8)
            }
            // The encoder takes 16-bit samples in native byte order.
            Pixels::Rgb16(p) => encoder.write_image(
                bytemuck::cast_slice(p),
                self.width,
                self.height,
                image::ExtendedColorType::Rgb16,
            ),
            Pixels::LinearF32(_) => {
                return Err(RenderError::Invalid(
                    "linear pixels can't be written as a PNG".into(),
                ));
            }
        };
        result.map_err(|e| RenderError::Invalid(format!("writing a PNG: {e}")))?;
        Ok(out)
    }

    /// A PNG read back, as RGB at its own depth (8 or 16 bits); alpha is
    /// dropped.
    pub fn from_png(bytes: &[u8]) -> Result<Image, RenderError> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|e| RenderError::Invalid(format!("reading a PNG: {e}")))?;
        let color = img.color();
        let deep = color.bytes_per_pixel() / color.channel_count() > 1;
        let (width, height) = (img.width(), img.height());
        let pixels = if deep {
            Pixels::Rgb16(img.into_rgb16().into_raw())
        } else {
            Pixels::Rgb8(img.into_rgb8().into_raw())
        };
        Ok(Image {
            width,
            height,
            pixels,
        })
    }
}

/// The pipelines and buffers of [`render_still`], kept for rendering
/// more than one still: one slot and one target, remade when a frame or a
/// size no longer fits.
#[derive(Debug)]
pub struct StillRenderer {
    uploader: Uploader,
    projector: Projector,
    encoder: Encoder,
    slot: Option<GpuFrame>,
    target: Option<LinearTarget>,
    output: Option<wgpu::Texture>,
}

impl StillRenderer {
    /// What the encode pass writes for [`Depth::Eight`].
    pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    pub fn new(gpu: &Gpu) -> StillRenderer {
        StillRenderer {
            uploader: Uploader::new(gpu),
            projector: Projector::new(gpu),
            encoder: Encoder::new(gpu, Self::OUTPUT_FORMAT),
            slot: None,
            target: None,
            output: None,
        }
    }

    pub fn projector(&self) -> &Projector {
        &self.projector
    }

    /// `frame` through `view`, `size` pixels large.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        view: &ViewParams,
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        self.render_from(gpu, frame, None, view, size, opts)
    }

    /// [`render`](Self::render) of a frame whose planes are on the GPU
    /// already, one texture a plane, as
    /// [`Uploader::upload_imported`](crate::Uploader::upload_imported)
    /// takes them: slice 5b's DMA-BUF imports. `frame` gives the layout,
    /// size and colour.
    pub fn render_imported(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        planes: &[wgpu::Texture],
        view: &ViewParams,
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        self.render_from(gpu, frame, Some(planes), view, size, opts)
    }

    fn render_from(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        planes: Option<&[wgpu::Texture]>,
        view: &ViewParams,
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        gpu.check()?;
        if size.0 == 0 || size.1 == 0 {
            return Err(RenderError::Invalid(format!(
                "a {}x{} still has no pixels",
                size.0, size.1
            )));
        }
        gpu.check_size(size)?;
        let shape = SlotShape::of(frame.layout(), opts.placement.shown(frame.size()))?;
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let slot = match self.slot.take() {
            Some(s) if s.shape().fits(&shape) => s,
            _ => GpuFrame::new(gpu, &shape)?,
        };
        let slot = &*self.slot.insert(slot);
        match planes {
            Some(planes) => {
                self.uploader
                    .upload_imported(gpu, frame, planes, &opts.placement, slot)?
            }
            None => self
                .uploader
                .upload_placed(gpu, frame, &opts.placement, slot)?,
        };
        let target = match self.target.take() {
            Some(mut t) => {
                t.resize(gpu, size)?;
                t
            }
            None => LinearTarget::new(gpu, size)?,
        };
        let target = &*self.target.insert(target);
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("still"),
            });
        self.projector
            .record(&mut enc, slot, view, target, opts.filter, opts.debug, None)?;
        let image = match opts.depth {
            Depth::Eight => {
                let output = match self.output.take() {
                    Some(t) if (t.width(), t.height()) == size => t,
                    _ => output_texture(gpu, size),
                };
                let output = &*self.output.insert(output);
                {
                    let view = output.create_view(&wgpu::TextureViewDescriptor::default());
                    let mut pass = crate::layouts::fullscreen_pass(&mut enc, "encode", &view);
                    self.encoder.draw(&mut pass, target, [0, 0]);
                }
                gpu.queue.submit([enc.finish()]);
                let texels = readback::read_texture(gpu, output, 0)?;
                rgb8(&texels)
            }
            Depth::Sixteen | Depth::Linear => {
                gpu.queue.submit([enc.finish()]);
                let texels = readback::read_region(gpu, target.texture(), 0, size)?;
                let linear = texels.f16_values();
                let rgb = linear.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]);
                let pixels = if opts.depth == Depth::Linear {
                    Pixels::LinearF32(rgb.collect())
                } else {
                    Pixels::Rgb16(
                        rgb.map(|l| (srgb_oetf(l.clamp(0.0, 1.0)) * 65535.0).round() as u16)
                            .collect(),
                    )
                };
                Image {
                    width: size.0,
                    height: size.1,
                    pixels,
                }
            }
        };
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Device(e.to_string()));
        }
        Ok(image)
    }
}

/// One frame, headless: upload, project, encode, read back. Makes its
/// pipelines each time; [`StillRenderer`] keeps them.
pub fn render_still(
    gpu: &Gpu,
    frame: &dyn VideoPlanes,
    view: &ViewParams,
    size: (u32, u32),
    opts: StillOptions,
) -> Result<Image, RenderError> {
    StillRenderer::new(gpu).render(gpu, frame, view, size, &opts)
}

fn output_texture(gpu: &Gpu, size: (u32, u32)) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("still output"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: StillRenderer::OUTPUT_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn rgb8(texels: &Texels) -> Image {
    Image {
        width: texels.width,
        height: texels.height,
        pixels: Pixels::Rgb8(
            texels
                .data
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect(),
        ),
    }
}

/// The sRGB OETF, as `shaders/encode.wgsl` applies it in f32. The shader
/// then rounds it to the output's codes itself, round(v · L) / L with
/// ties to even for L = 255 or 1023 ([`crate::encode::code_max`]), at
/// 1023 as its nearest half ([`crate::encode::returns_half`]), so that
/// a driver whose unorm conversion rounds to nearest, as Vulkan asks,
/// stores k; the 16-bit path rounds it to 65535 on the CPU instead.
pub fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The sRGB EOTF, as `shaders/project.wgsl` applies it.
pub fn srgb_eotf(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pngs_round_trip_at_both_depths() {
        let eight = Image {
            width: 3,
            height: 2,
            pixels: Pixels::Rgb8((0..18).map(|v| v * 14).collect()),
        };
        assert_eq!(Image::from_png(&eight.to_png().unwrap()).unwrap(), eight);
        let sixteen = Image {
            width: 2,
            height: 3,
            pixels: Pixels::Rgb16((0..18).map(|v| v * 3600 + 7).collect()),
        };
        assert_eq!(
            Image::from_png(&sixteen.to_png().unwrap()).unwrap(),
            sixteen
        );
        let linear = Image {
            width: 1,
            height: 1,
            pixels: Pixels::LinearF32(vec![0.5; 3]),
        };
        assert_eq!(linear.to_png().unwrap_err().code(), "invalid_input");
        assert_eq!(
            Image::from_png(b"not a png").unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn transfer_functions_invert() {
        for i in 0..=1000 {
            let v = i as f32 / 1000.0;
            assert!((srgb_oetf(srgb_eotf(v)) - v).abs() < 1e-5, "{v}");
        }
    }
}
