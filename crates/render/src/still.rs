//! One frame, headless (M1 plan section 4.5): upload, project, encode and
//! read back. `spherecut frame` and the goldens use it.

use std::time::{Duration, Instant};

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
use crate::upload::{UploadPath, Uploader};
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
    /// The YUV pass's output and the buffer it is read back through,
    /// kept while frames are the same size: an 8K P010 frame's are 88 MB
    /// each, a lot to make and free a frame.
    yuv_out: Option<wgpu::Buffer>,
    staging: Option<wgpu::Buffer>,
    pacing: Pacing,
    /// The last [`render_yuv`](Self::render_yuv)'s read back.
    readback: Duration,
}

/// The largest band of a view drawn in a submission of its own while
/// the renderer shares the device ([`StillRenderer::share`]): a 4K
/// frame's pixels, about 2 ms of RADV's on odin's RX 7600.
pub const SHARED_BAND_PIXELS: u64 = 3840 * 2160;

/// The largest copy a read back makes in one submission while shared: on
/// PCIe 4.0 ×8, about 1.3 ms.
pub const SHARED_COPY_BYTES: u64 = 16 << 20;

/// How a renderer's submissions go: one after another as fast as the
/// queue takes them, or, shared, each after the one before has finished.
#[derive(Debug)]
struct Pacing {
    shared: bool,
    /// A submission of this renderer's may not be done.
    pending: bool,
    band_pixels: u64,
    copy_bytes: u64,
}

impl Default for Pacing {
    fn default() -> Pacing {
        Pacing {
            shared: false,
            pending: false,
            band_pixels: SHARED_BAND_PIXELS,
            copy_bytes: SHARED_COPY_BYTES,
        }
    }
}

impl Pacing {
    /// Submits `enc` once the device's submit gate is open and, while
    /// shared, once this renderer's last submission is done.
    fn submit(&mut self, gpu: &Gpu, enc: wgpu::CommandEncoder) -> Result<(), RenderError> {
        if self.shared && self.pending {
            self.finish(gpu)?;
        }
        gpu.submit_gate().wait();
        gpu.queue.submit([enc.finish()]);
        self.pending = true;
        Ok(())
    }

    /// Waits for this renderer's submissions to be done.
    fn finish(&mut self, gpu: &Gpu) -> Result<(), RenderError> {
        if self.pending {
            gpu.wait_for_submitted()?;
            self.pending = false;
        }
        Ok(())
    }

    /// The rows of a `size` view, in bands while shared: each band at
    /// most [`SHARED_BAND_PIXELS`] and an even number of rows, so that
    /// no 2×2 quad straddles two.
    fn bands(&self, size: (u32, u32)) -> Vec<std::ops::Range<u32>> {
        let (w, h) = size;
        let rows = if self.shared {
            u32::try_from(self.band_pixels / u64::from(w.max(1)))
                .unwrap_or(u32::MAX)
                .max(2)
                & !1
        } else {
            h
        };
        (0..h)
            .step_by(rows as usize)
            .map(|y| y..(y + rows).min(h))
            .collect()
    }
}

impl StillRenderer {
    /// What the encode pass writes for [`Depth::Eight`].
    pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    pub fn new(gpu: &Gpu) -> StillRenderer {
        StillRenderer::with_upload_path(gpu, UploadPath::default())
    }

    /// A renderer whose frames reach the GPU by `path`. A renderer that
    /// shares the device with a window ([`Self::share`]) takes the
    /// staging belt, as the window's player does: `Queue::write_texture`
    /// makes its staging buffer and copies a frame into it holding the
    /// lock the window's present needs, 10 ms or more for a 5.7K frame.
    pub fn with_upload_path(gpu: &Gpu, path: UploadPath) -> StillRenderer {
        StillRenderer {
            uploader: Uploader::with_path(gpu, path),
            projector: Projector::new(gpu),
            encoder: Encoder::new(gpu, Self::OUTPUT_FORMAT),
            yuv: None,
            blur: None,
            slot: None,
            target: None,
            output: None,
            yuv_out: None,
            staging: None,
            pacing: Pacing::default(),
            readback: Duration::ZERO,
        }
    }

    /// Shares the device with a window that plays (M2 plan item 22, as
    /// odin's M2 run measured it): each of the renderer's submissions
    /// waits for the one before it to finish, a large view is drawn in
    /// bands and read back in parts, so that the window's frame queues
    /// behind one band's work at most, not a frame's. Every submission
    /// waits at the device's submit gate (D41), shared or not.
    pub fn share(&mut self, shared: bool) {
        self.pacing.shared = shared;
    }

    /// The parts a shared renderer works in: bands of `band_pixels` (a
    /// row's at the least, two rows'), and copies of `copy_bytes` (a
    /// multiple of 4, at least 4). [`SHARED_BAND_PIXELS`] and
    /// [`SHARED_COPY_BYTES`] unless set; the tests set small ones.
    pub fn share_in_parts(&mut self, band_pixels: u64, copy_bytes: u64) {
        self.pacing.band_pixels = band_pixels;
        self.pacing.copy_bytes = (copy_bytes & !3).max(wgpu::COPY_BUFFER_ALIGNMENT);
    }

    /// How long the last [`render_yuv`](Self::render_yuv) took to copy
    /// its planes back once the GPU had drawn them: the rest of the call
    /// is the upload and the draw.
    pub fn last_readback(&self) -> Duration {
        self.readback
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

    /// [`render_blurred`](Self::render_blurred) of a frame whose planes
    /// are on the GPU already, one texture a plane, as
    /// [`Uploader::upload_imported`](crate::Uploader::upload_imported)
    /// takes them: slice 5b's DMA-BUF imports. `frame` gives the layout,
    /// size and colour.
    pub fn render_imported(
        &mut self,
        gpu: &Gpu,
        frame: &dyn VideoPlanes,
        planes: &[wgpu::Texture],
        views: &[ViewParams],
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        let want = Want::Image(opts.depth);
        let source = Source::Imported(planes);
        Ok(image(self.render_from(
            gpu, frame, source, views, size, opts, want,
        )?))
    }

    /// [`render_blurred`](Self::render_blurred) of a frame whose shown
    /// planes are in a GPU buffer, laid out as `planes` says, as
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
        views: &[ViewParams],
        size: (u32, u32),
        opts: &StillOptions,
    ) -> Result<Image, RenderError> {
        let want = Want::Image(opts.depth);
        let source = Source::Buffer(buffer, planes);
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
            let bands = self.pacing.bands(size);
            if let [_] = bands.as_slice() {
                self.projector.record(
                    &mut enc,
                    slot,
                    view,
                    target,
                    opts.filter,
                    opts.debug,
                    None,
                )?;
            } else {
                for rows in bands {
                    self.projector.record_rows(
                        &mut enc,
                        slot,
                        view,
                        target,
                        opts.filter,
                        opts.debug,
                        rows,
                    )?;
                    self.pacing.submit(gpu, enc)?;
                    enc = gpu
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("still"),
                        });
                }
            }
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
                self.pacing.submit(gpu, enc)?;
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
                let out = match self.yuv_out.take() {
                    Some(b) if b.size() == shape.bytes => b,
                    _ => YuvConverter::buffer(gpu, &shape),
                };
                let out = &*self.yuv_out.insert(out);
                converter.record(gpu, &mut enc, target.texture_view(), &shape, out)?;
                self.pacing.submit(gpu, enc)?;
                // The draw finishes before the copy starts, so the copy
                // is timed alone.
                self.pacing.finish(gpu)?;
                let read = Instant::now();
                let data = read_back(gpu, &mut self.pacing, &mut self.staging, out, shape.bytes)?;
                self.readback = read.elapsed();
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
                self.pacing.submit(gpu, enc)?;
                let texels = readback::read_texture(gpu, output, 0)?;
                Rendered::Image(rgb8(&texels))
            }
            (None, Depth::Sixteen | Depth::Linear) => {
                self.pacing.submit(gpu, enc)?;
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

/// `bytes` of `buffer`, through `staging` (made, or made again at
/// another size), copied in parts of [`SHARED_COPY_BYTES`] while shared.
fn read_back(
    gpu: &Gpu,
    pacing: &mut Pacing,
    staging: &mut Option<wgpu::Buffer>,
    buffer: &wgpu::Buffer,
    bytes: u64,
) -> Result<Vec<u8>, RenderError> {
    spherecut_core::may_block("reading a frame back from the GPU");
    let device = &gpu.device;
    // Kept only once read and unmapped.
    let buf = match staging.take() {
        Some(b) if b.size() == bytes => b,
        _ => device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("still readback"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
    };
    let part = if pacing.shared {
        pacing.copy_bytes
    } else {
        bytes
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let mut tx = Some(tx);
    let mut at = 0;
    while at < bytes {
        let n = part.min(bytes - at);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("still readback"),
        });
        enc.copy_buffer_to_buffer(buffer, at, &buf, at, n);
        at += n;
        if at == bytes
            && let Some(tx) = tx.take()
        {
            enc.map_buffer_on_submit(&buf, wgpu::MapMode::Read, .., move |r| {
                let _ = tx.send(r);
            });
        }
        pacing.submit(gpu, enc)?;
    }
    pacing.finish(gpu)?;
    rx.recv()
        .map_err(|_| RenderError::Readback("the copy never finished".into()))?
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    let data = buf
        .get_mapped_range(..)
        .map_err(|e| RenderError::Readback(e.to_string()))?
        .to_vec();
    buf.unmap();
    *staging = Some(buf);
    Ok(data)
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
