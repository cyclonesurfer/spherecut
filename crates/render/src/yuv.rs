//! The YUV pass (M2 plan section 7.3, amendment 2 item 6): a linear
//! target into the encoders' Y′CbCr, BT.709 in limited range, 4:2:0 with
//! left siting, in a storage buffer laid out as the encoder takes it, so
//! that each row maps into an `AVFrame`'s plane with one copy. The CPU
//! never converts a frame; [`reference`] does the same in f64 for the
//! tests.

use wgpu::util::DeviceExt;

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::layouts;

/// What the encoders take (amendment 2 item 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YuvLayout {
    /// 8-bit, Y then interleaved CbCr: VA-API's.
    Nv12,
    /// 10-bit as NV12, each code in the top 10 bits of a little-endian
    /// `u16`: VA-API's.
    P010,
    /// 8-bit, Y, Cb, Cr: the software encoders'.
    I420,
    /// 10-bit as I420, each code in the low 10 bits of a little-endian
    /// `u16`: the software encoders'.
    I010,
}

impl YuvLayout {
    pub const ALL: [YuvLayout; 4] = [
        YuvLayout::Nv12,
        YuvLayout::P010,
        YuvLayout::I420,
        YuvLayout::I010,
    ];

    pub fn bits(self) -> u32 {
        match self {
            YuvLayout::Nv12 | YuvLayout::I420 => 8,
            YuvLayout::P010 | YuvLayout::I010 => 10,
        }
    }

    /// Cb and Cr in planes of their own, rather than interleaved.
    pub fn planar(self) -> bool {
        matches!(self, YuvLayout::I420 | YuvLayout::I010)
    }

    /// Bytes a sample.
    pub fn sample_bytes(self) -> u32 {
        if self.bits() == 8 { 1 } else { 2 }
    }

    /// Where a 10-bit code sits in its `u16`.
    fn shift(self) -> u32 {
        match self {
            YuvLayout::P010 => 6,
            _ => 0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            YuvLayout::Nv12 => "nv12",
            YuvLayout::P010 => "p010",
            YuvLayout::I420 => "i420",
            YuvLayout::I010 => "i010",
        }
    }
}

/// Every row of every plane is padded to this many bytes.
pub const ROW_ALIGN: u32 = 256;

/// One plane in the pass's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvPlane {
    /// Its first byte.
    pub offset: u64,
    /// Bytes from one row to the next, a multiple of [`ROW_ALIGN`].
    pub stride: u32,
    /// The bytes of a row that hold samples.
    pub row_bytes: u32,
    pub rows: u32,
}

/// A picture's planes in the pass's buffer: Y, then CbCr or Cb and Cr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvShape {
    pub layout: YuvLayout,
    pub size: (u32, u32),
    pub planes: Vec<YuvPlane>,
    /// The buffer's size.
    pub bytes: u64,
}

impl YuvShape {
    /// `size` in `layout`. `Invalid` unless the width is a multiple of 4
    /// and the height even, as every preset is (M2 plan section 8.1).
    pub fn new(layout: YuvLayout, size: (u32, u32)) -> Result<YuvShape, RenderError> {
        let (w, h) = size;
        if w == 0 || h == 0 || w % 4 != 0 || h % 2 != 0 {
            return Err(RenderError::Invalid(format!(
                "a {w}x{h} picture can't be 4:2:0 here: the width must be a multiple of 4 and \
                 the height even"
            )));
        }
        let b = layout.sample_bytes();
        let mut rows = vec![(w * b, h)];
        if layout.planar() {
            rows.extend([(w / 2 * b, h / 2); 2]);
        } else {
            rows.push((w * b, h / 2));
        }
        let mut offset = 0u64;
        let planes = rows
            .into_iter()
            .map(|(row_bytes, rows)| {
                let stride = row_bytes.next_multiple_of(ROW_ALIGN);
                let p = YuvPlane {
                    offset,
                    stride,
                    row_bytes,
                    rows,
                };
                offset += u64::from(stride) * u64::from(rows);
                p
            })
            .collect();
        Ok(YuvShape {
            layout,
            size,
            planes,
            bytes: offset,
        })
    }

    /// The samples of row `y` of plane `p` in `buffer`, the pass's output
    /// as read back.
    pub fn row<'b>(&self, buffer: &'b [u8], p: usize, y: u32) -> &'b [u8] {
        let plane = &self.planes[p];
        let start = (plane.offset + u64::from(y) * u64::from(plane.stride)) as usize;
        &buffer[start..start + plane.row_bytes as usize]
    }
}

/// The shader's `Yuv` uniform (`shaders/yuv.wgsl`; a naga test checks
/// every offset).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct YuvUniform {
    pub offsets: [u32; 4],
    pub strides: [u32; 4],
    pub size: [u32; 2],
    pub bits: u32,
    pub planar: u32,
    pub shift: u32,
    pub _pad: [u32; 3],
}

impl YuvUniform {
    pub fn new(shape: &YuvShape) -> YuvUniform {
        let mut offsets = [0; 4];
        let mut strides = [0; 4];
        for (i, p) in shape.planes.iter().enumerate() {
            // Both are multiples of ROW_ALIGN: whole words.
            offsets[i] = (p.offset / 4) as u32;
            strides[i] = p.stride / 4;
        }
        YuvUniform {
            offsets,
            strides,
            size: [shape.size.0, shape.size.1],
            bits: shape.layout.bits(),
            planar: u32::from(shape.layout.planar()),
            shift: shape.layout.shift(),
            _pad: [0; 3],
        }
    }
}

/// The pass's two pipelines, for every layout.
#[derive(Debug)]
pub struct YuvConverter {
    layout: wgpu::BindGroupLayout,
    luma: wgpu::ComputePipeline,
    chroma: wgpu::ComputePipeline,
}

const WORKGROUP: u32 = 64;

impl YuvConverter {
    pub fn new(gpu: &Gpu) -> YuvConverter {
        let device = &gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(size_of::<YuvUniform>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let module = layouts::module(device, "yuv", layouts::YUV_WGSL);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        YuvConverter {
            luma: pipeline("cs_luma"),
            chroma: pipeline("cs_chroma"),
            layout,
        }
    }

    /// A buffer the pass can write `shape` into and that can be copied
    /// from.
    pub fn buffer(gpu: &Gpu, shape: &YuvShape) -> wgpu::Buffer {
        gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuv"),
            size: shape.bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        })
    }

    /// Records the conversion of `linear`'s top-left `shape.size` into
    /// `out`, which holds `shape.bytes` at least, made by
    /// [`YuvConverter::buffer`]. `Invalid` when `out` is too small.
    pub fn record(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        linear: &wgpu::TextureView,
        shape: &YuvShape,
        out: &wgpu::Buffer,
    ) -> Result<(), RenderError> {
        if out.size() < shape.bytes {
            return Err(RenderError::Invalid(format!(
                "a {}-byte buffer can't hold {} bytes of {}",
                out.size(),
                shape.bytes,
                shape.layout.name()
            )));
        }
        let uniform = YuvUniform::new(shape);
        let device = &gpu.device;
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuv uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(linear),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("yuv"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &group, &[]);
        pass.set_pipeline(&self.luma);
        pass.dispatch_workgroups(uniform.strides[0].div_ceil(WORKGROUP), shape.size.1, 1);
        pass.set_pipeline(&self.chroma);
        pass.dispatch_workgroups(uniform.strides[1].div_ceil(WORKGROUP), shape.size.1 / 2, 1);
        Ok(())
    }
}

/// Reads `bytes` of `buffer` back, waiting for the GPU.
pub fn read_buffer(gpu: &Gpu, buffer: &wgpu::Buffer, bytes: u64) -> Result<Vec<u8>, RenderError> {
    spherecut_core::may_block("reading a buffer back from the GPU");
    gpu.check()?;
    let device = &gpu.device;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("yuv readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("yuv readback"),
    });
    enc.copy_buffer_to_buffer(buffer, 0, &staging, 0, bytes);
    let (tx, rx) = std::sync::mpsc::channel();
    enc.map_buffer_on_submit(&staging, wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    gpu.submit_gate().wait();
    gpu.queue.submit([enc.finish()]);
    let read = || -> Result<Vec<u8>, RenderError> {
        gpu.wait_for_submitted()?;
        rx.recv()
            .map_err(|_| RenderError::Readback("the copy never finished".into()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let data = staging
            .get_mapped_range(..)
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .to_vec();
        staging.unmap();
        Ok(data)
    };
    let data = read();
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(RenderError::Readback(e.to_string()));
    }
    data
}

/// What the pass computes before it rounds, in f64, at 8-bit scale (×4
/// for 10 bits): each pixel's Y, and each chroma sample's Cb and Cr, rows
/// top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub struct Codes {
    pub size: (u32, u32),
    pub y: Vec<f64>,
    pub cb: Vec<f64>,
    pub cr: Vec<f64>,
}

/// The pass in f64: `linear` is R, G, B per pixel in linear light, as
/// the target holds it, `size` pixels.
pub fn reference(linear: &[f64], size: (u32, u32)) -> Codes {
    let (w, h) = (size.0 as i64, size.1 as i64);
    let encoded = |x: i64, y: i64| -> [f64; 3] {
        let i = 3 * (y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize;
        [0, 1, 2].map(|c| crate::reference::srgb_oetf(linear[i + c].clamp(0.0, 1.0)))
    };
    let luma = |e: [f64; 3]| 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
    let chroma = |e: [f64; 3]| {
        let y = luma(e);
        [(e[2] - y) / 1.8556, (e[0] - y) / 1.5748]
    };
    let mut y = Vec::with_capacity((w * h) as usize);
    for j in 0..h {
        for i in 0..w {
            y.push(16.0 + 219.0 * luma(encoded(i, j)));
        }
    }
    let (mut cb, mut cr) = (Vec::new(), Vec::new());
    for cy in 0..h / 2 {
        for cx in 0..w / 2 {
            let mut s = [0.0; 2];
            for r in 0..2 {
                let (x, row) = (2 * cx, 2 * cy + r);
                let [a, b, c] = [x - 1, x, x + 1].map(|x| chroma(encoded(x, row)));
                for k in 0..2 {
                    s[k] += 0.25 * a[k] + 0.5 * b[k] + 0.25 * c[k];
                }
            }
            cb.push(128.0 + 224.0 * s[0] * 0.5);
            cr.push(128.0 + 224.0 * s[1] * 0.5);
        }
    }
    Codes { size, y, cb, cr }
}

/// `v` (at 8-bit scale) as a code of `bits`, ties to even.
pub fn to_code(v: f64, bits: u32) -> u32 {
    let scaled = if bits == 10 { v * 4.0 } else { v };
    let max = if bits == 10 { 1023.0 } else { 255.0 };
    scaled.round_ties_even().clamp(0.0, max) as u32
}

/// A sample of plane `p` (0 Y, 1 Cb, 2 Cr) at `(x, y)` of the pass's
/// output `buffer` (`x`, `y` in that plane's samples).
pub fn sample(shape: &YuvShape, buffer: &[u8], p: usize, x: u32, y: u32) -> u32 {
    let l = shape.layout;
    let b = l.sample_bytes() as usize;
    // Interleaved chroma: Cb then Cr in plane 1.
    let (plane, index) = match (p, l.planar()) {
        (0, _) => (0, x as usize),
        (_, true) => (p, x as usize),
        (_, false) => (1, 2 * x as usize + (p - 1)),
    };
    let row = shape.row(buffer, plane, y);
    let at = &row[index * b..index * b + b];
    if b == 1 {
        u32::from(at[0])
    } else {
        u32::from(u16::from_le_bytes([at[0], at[1]])) >> l.shift()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_planes_are_where_the_encoders_want_them() {
        let s = YuvShape::new(YuvLayout::Nv12, (1920, 1080)).unwrap();
        assert_eq!(
            s.planes,
            [
                YuvPlane {
                    offset: 0,
                    stride: 2048,
                    row_bytes: 1920,
                    rows: 1080
                },
                YuvPlane {
                    offset: 2048 * 1080,
                    stride: 2048,
                    row_bytes: 1920,
                    rows: 540
                },
            ]
        );
        let s = YuvShape::new(YuvLayout::P010, (1080, 1920)).unwrap();
        assert_eq!((s.planes[0].stride, s.planes[0].row_bytes), (2304, 2160));
        assert_eq!(s.planes[1].rows, 960);
        let s = YuvShape::new(YuvLayout::I420, (3840, 2160)).unwrap();
        assert_eq!(s.planes.len(), 3);
        assert_eq!((s.planes[1].row_bytes, s.planes[1].stride), (1920, 2048));
        assert_eq!(s.planes[2].offset, s.planes[1].offset + 2048 * 1080);
        assert_eq!(s.bytes, s.planes[2].offset + 2048 * 1080);
        let s = YuvShape::new(YuvLayout::I010, (4, 2)).unwrap();
        assert_eq!(
            s.planes.iter().map(|p| p.stride).collect::<Vec<_>>(),
            [256; 3]
        );
        for bad in [(1922, 1080), (1920, 1081), (0, 2)] {
            assert!(YuvShape::new(YuvLayout::Nv12, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn codes_round_ties_to_even_and_scale_to_ten_bits() {
        assert_eq!((to_code(16.5, 8), to_code(17.5, 8)), (16, 18));
        assert_eq!(to_code(235.0, 10), 940);
        assert_eq!(to_code(16.125, 10), 64);
        assert_eq!(to_code(16.375, 10), 66);
        assert_eq!(to_code(300.0, 8), 255);
    }

    #[test]
    fn black_white_and_grey_are_their_codes() {
        // Black, white and a mid grey, 4 × 2 each.
        for (l, want_y) in [(0.0, 16.0), (1.0, 235.0)] {
            let c = reference(&[l; 4 * 2 * 3], (4, 2));
            assert!(c.y.iter().all(|y| (y - want_y).abs() < 1e-12), "{c:?}");
            assert!(c.cb.iter().chain(&c.cr).all(|v| (v - 128.0).abs() < 1e-12));
        }
        // Pure red: Cr at its top, Cb below the middle.
        let red: Vec<f64> = (0..8).flat_map(|_| [1.0, 0.0, 0.0]).collect();
        let c = reference(&red, (4, 2));
        assert!((c.y[0] - (16.0 + 219.0 * 0.2126)).abs() < 1e-12);
        assert!((c.cr[0] - 240.0).abs() < 1e-12, "{}", c.cr[0]);
        assert!((c.cb[0] - (128.0 - 224.0 * 0.2126 / 1.8556)).abs() < 1e-12);
    }
}
