//! One frame, headless (M1 plan section 4.5): upload, project, encode and
//! read back. `spherecut frame` and the goldens use it.

use spherecut_core::video::VideoPlanes;

use crate::blur::Blur;
use crate::color::Placement;
use crate::encode::Encoder;
use crate::error::RenderError;
use crate::frame::GpuFrame;
use crate::gpu::Gpu;
use crate::plane::SlotShape;
use crate::project::{DebugMode, Filter, LinearTarget, Projector, ViewParams};
use crate::readback::{self, Texels};
use crate::upload::Uploader;
use crate::vkshare::BufferPlane;
use crate::yuv::{self, YuvConverter, YuvLayout, YuvShape};

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
    /// The picture as coded, scaled to the size, as the GUI's raw view
    /// draws it ([`Projector::record_flat_unchecked`]): the view and
    /// `debug` go unused.
    pub flat: bool,
}

/// A rendered picture's pixels: R, G, B per pixel, rows top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub enum Pixels {
    Rgb8(Vec<u8>),
    Rgb16(Vec<u16>),
    /// Linear values ([`Depth::Linear`]).
    LinearF32(Vec<f32>),
}

/// A picture as an encoder takes it: [`YuvConverter`]'s output, read
/// back.
#[derive(Debug, Clone, PartialEq)]
pub struct YuvImage {
    pub shape: YuvShape,
    /// The planes, laid out as `shape` says.
    pub data: Vec<u8>,
}

impl YuvImage {
    /// Sample `(x, y)` of plane `p`: 0 Y, 1 Cb, 2 Cr, in that plane's
    /// samples.
    pub fn sample(&self, p: usize, x: u32, y: u32) -> u32 {
        yuv::sample(&self.shape, &self.data, p, x, y)
    }
}

/// What [`StillRenderer`] reads back.
enum Want {
    Image(Depth),
    Yuv(YuvLayout),
}

enum Rendered {
    Image(Image),
    Yuv(YuvImage),
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
    /// Made when first asked for a [`YuvImage`].
    yuv: Option<YuvConverter>,
    /// Made when first asked to blur.
    blur: Option<Blur>,
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
            yuv: None,
            blur: None,
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
        let want = Want::Image(opts.depth);
        let views = std::slice::from_ref(view);
        Ok(image(self.render_from(
            gpu,
            frame,
            Source::Bytes,
            views,
            size,
            opts,
            want,
        )?))
    }

    /// `frame` seen through each of `views` (a frame's views over the
    /// shutter, [`crate::blur`]) and averaged in linear light, `size`
    /// pixels large: motion blur. One view is [`Self::render`].
    pub fn render_blurred(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        views: &[ViewParams],
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        let want = Want::Image(opts.depth);
        Ok(image(self.render_from(
            gpu,
            frame,
            Source::Bytes,
            views,
            size,
            opts,
            want,
        )?))
    }

    /// `frame` through `views`, averaged as [`Self::render_blurred`]
    /// does, `size` pixels large, as an encoder's planes in `layout`: the
    /// YUV pass's output, which the export's parity tests compare against
    /// (M2 plan amendment 2 item 5). `opts.depth` goes unused.
    pub fn render_yuv(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        views: &[ViewParams],
        size: (u32, u32),
        opts: &StillOptions,
        layout: YuvLayout,
    ) -> Result<YuvImage, RenderError> {
        let want = Want::Yuv(layout);
        match self.render_from(gpu, frame, Source::Bytes, views, size, opts, want)? {
            Rendered::Yuv(y) => Ok(y),
            Rendered::Image(_) => unreachable!("a YUV render reads back planes"),
        }
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
        let want = Want::Image(opts.depth);
        let (views, source) = (std::slice::from_ref(view), Source::Imported(planes));
        Ok(image(self.render_from(
            gpu, frame, source, views, size, opts, want,
        )?))
    }

    /// [`render`](Self::render) of a frame whose shown planes are in a
    /// GPU buffer, laid out as `planes` says, as
    /// [`Uploader::upload_from_buffer`](crate::Uploader::upload_from_buffer)
    /// takes them: slice 5b's copies out of FFmpeg's Vulkan images.
    /// `frame` gives the layout, size and colour.
    #[allow(clippy::too_many_arguments)]
    pub fn render_copied(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        buffer: &wgpu::Buffer,
        planes: &[BufferPlane],
        view: &ViewParams,
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        let want = Want::Image(opts.depth);
        let (views, source) = (std::slice::from_ref(view), Source::Buffer(buffer, planes));
        Ok(image(self.render_from(
            gpu, frame, source, views, size, opts, want,
        )?))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_from(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        source: Source<'_>,
        views: &[ViewParams],
        size: (u32, u32),
        opts: &StillOptions,
        want: Want,
    ) -> Result<Rendered, RenderError> {
        gpu.check()?;
        if views.is_empty() || (opts.flat && views.len() > 1) {
            return Err(RenderError::Invalid(format!(
                "a still takes one view, or several to blur through the lens, not {}",
                views.len()
            )));
        }
        let yuv_shape = match want {
            Want::Yuv(layout) => Some(YuvShape::new(layout, size)?),
            Want::Image(_) => None,
        };
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
        match source {
            Source::Bytes => self
                .uploader
                .upload_placed(gpu, frame, &opts.placement, slot)?,
            Source::Imported(planes) => {
                self.uploader
                    .upload_imported(gpu, frame, planes, &opts.placement, slot)?
            }
            Source::Buffer(buffer, planes) => self.uploader.upload_from_buffer(
                gpu,
                frame,
                buffer,
                planes,
                &opts.placement,
                slot,
            )?,
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
        if opts.flat {
            gpu.check()?;
            self.projector
                .record_flat_unchecked(&mut enc, slot, target, opts.filter, None);
        } else if let [view] = views {
            self.projector
                .record(&mut enc, slot, view, target, opts.filter, opts.debug, None)?;
        } else {
            // Each view is projected and added in a submission of its own,
            // since the view uniform is written through the queue.
            let blur = self.blur.get_or_insert_with(|| Blur::new(gpu));
            blur.begin(gpu, &mut enc, size);
            for view in views {
                self.projector.record(
                    &mut enc,
                    slot,
                    view,
                    target,
                    opts.filter,
                    opts.debug,
                    None,
                )?;
                blur.add(gpu, &mut enc, target)?;
                gpu.queue.submit([enc.finish()]);
                enc = gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("still"),
                    });
            }
            blur.resolve(gpu, &mut enc, target, views.len() as u32)?;
        }
        let depth = match want {
            Want::Image(d) => d,
            Want::Yuv(_) => Depth::Linear,
        };
        let image = match (yuv_shape, depth) {
            (Some(shape), _) => {
                let converter = self.yuv.get_or_insert_with(|| YuvConverter::new(gpu));
                let out = YuvConverter::buffer(gpu, &shape);
                converter.record(gpu, &mut enc, target.texture_view(), &shape, &out)?;
                gpu.queue.submit([enc.finish()]);
                let data = yuv::read_buffer(gpu, &out, shape.bytes)?;
                Rendered::Yuv(YuvImage { shape, data })
            }
            (None, Depth::Eight) => {
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
                Rendered::Image(rgb8(&texels))
            }
            (None, Depth::Sixteen | Depth::Linear) => {
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
                Rendered::Image(Image {
                    width: size.0,
                    height: size.1,
                    pixels,
                })
            }
        };
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(RenderError::Device(e.to_string()));
        }
        Ok(image)
    }
}

fn image(r: Rendered) -> Image {
    match r {
        Rendered::Image(i) => i,
        Rendered::Yuv(_) => unreachable!("an image render reads back an image"),
    }
}

/// Where a still's planes come from.
enum Source<'a> {
    /// The frame's own bytes.
    Bytes,
    /// Textures on the GPU, one a plane.
    Imported(&'a [wgpu::Texture]),
    /// A buffer on the GPU holding the shown planes.
    Buffer(&'a wgpu::Buffer, &'a [BufferPlane]),
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
