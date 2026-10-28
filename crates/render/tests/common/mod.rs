//! Shared by the render tests: one lavapipe device per test binary, and
//! frames in every plane layout the renderer samples, made from synth's
//! patterns with padded rows.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::OnceLock;

use spherecut_core::video::{
    Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, PixelLayout, PlaneData,
    PlaneDesc, Primaries, Range, SampleBytes, Transfer, VideoPlanes,
};
use spherecut_render::{AdapterChoice, Gpu, RenderError};
use spherecut_synth::RgbImage;
use spherecut_synth::yuv::{self, Chroma, YuvFormat};
use spherecut_testkit::tier2;

/// The CPU adapter, as every Tier 1 test renders on: made once, shared by
/// the binary's tests.
pub fn gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(|| {
        let gpu = Gpu::headless(&AdapterChoice::Cpu)
            .expect("lavapipe, Mesa's Vulkan CPU driver, is needed for the render tests");
        assert_eq!(
            gpu.caps.info.device_type,
            wgpu::DeviceType::Cpu,
            "{:?}",
            gpu.caps.info
        );
        gpu
    })
}

/// Every Vulkan adapter, described, and a device on each one that isn't
/// a CPU: what the Tier 2 render tests run on. A GPU that won't open a
/// device fails the test.
pub fn hardware_gpus() -> (Vec<Gpu>, Vec<String>) {
    let instance = wgpu::Instance::new(Gpu::instance_descriptor());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    let mut gpus = Vec::new();
    let mut seen = Vec::new();
    for a in adapters {
        let info = a.get_info();
        seen.push(format!(
            "{} ({:?}, {} {})",
            info.name, info.device_type, info.driver, info.driver_info
        ));
        if info.device_type == wgpu::DeviceType::Cpu {
            continue;
        }
        let name = if info.device_pci_bus_id.is_empty() {
            info.name.clone()
        } else {
            info.device_pci_bus_id.clone()
        };
        match Gpu::headless(&AdapterChoice::Named(name)) {
            Ok(g) => gpus.push(g),
            Err(e) => panic!("{} won't open a device: {e}", info.name),
        }
    }
    (gpus, seen)
}

/// The GPU a Tier 2 test runs on, or a skip: the adapter
/// `SPHERECUT_ADAPTER` names, else the first GPU Vulkan lists (the loader
/// puts discrete GPUs first).
pub fn hardware_gpu() -> Gpu {
    tier2::require_hw_run();
    if let Some(choice) = AdapterChoice::from_env() {
        return named_gpu(&choice, Gpu::headless(&choice));
    }
    let (gpus, seen) = hardware_gpus();
    gpus.into_iter()
        .next()
        .unwrap_or_else(|| tier2::skip(format!("no GPU: Vulkan lists only {}", seen.join(", "))))
}

/// What opening the adapter `choice` names gave: the GPU, or a skip when
/// there is no such adapter or it is a CPU. An adapter that is there but
/// won't give a device fails the test, as in [`hardware_gpus`]: that is a
/// fault to report, not missing hardware.
pub fn named_gpu(choice: &AdapterChoice, opened: Result<Gpu, RenderError>) -> Gpu {
    let env = AdapterChoice::ENV;
    match opened {
        Ok(gpu) if gpu.caps.is_cpu => tier2::skip(format!(
            "{env}={choice} is a CPU adapter, {}",
            gpu.caps.info.name
        )),
        Ok(gpu) => gpu,
        Err(e @ (RenderError::NoAdapter { .. } | RenderError::AdapterNotFound { .. })) => {
            tier2::skip(format!("{env}={choice}: {e}"))
        }
        Err(e) => panic!("{env}={choice} won't open a device: {e}"),
    }
}

/// A decoded picture held in memory, with rows `pad` bytes longer than
/// they need be and no padding after the last.
#[derive(Debug, Clone)]
pub struct Frame {
    pub layout: PixelLayout,
    pub size: (u32, u32),
    pub planes: Vec<(Vec<u8>, usize, (u32, u32))>,
    pub color: ColorInfo,
}

impl VideoPlanes for Frame {
    fn layout(&self) -> &PixelLayout {
        &self.layout
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn plane(&self, index: usize) -> Option<PlaneData<'_>> {
        self.planes
            .get(index)
            .map(|(bytes, stride, (w, h))| PlaneData {
                bytes,
                stride: *stride,
                width: *w,
                height: *h,
            })
    }

    fn color(&self) -> &ColorInfo {
        &self.color
    }
}

/// The plane layouts the tests cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Nv12,
    P010,
    I420,
    I420p10,
    I422,
    I422p10,
    I444,
    I444p12,
    Gray,
    /// 16-bit grey, the bits low: the deepest a plane comes.
    Gray16,
    Gbrp,
    /// NV12's 4:4:4 twin: an `Rg8` plane as large as luma.
    Nv24,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Nv12 => "nv12",
            Kind::P010 => "p010le",
            Kind::I420 => "yuv420p",
            Kind::I420p10 => "yuv420p10le",
            Kind::I422 => "yuv422p",
            Kind::I422p10 => "yuv422p10le",
            Kind::I444 => "yuv444p",
            Kind::I444p12 => "yuv444p12le",
            Kind::Gray => "gray",
            Kind::Gray16 => "gray16le",
            Kind::Gbrp => "gbrp",
            Kind::Nv24 => "nv24",
        }
    }

    pub fn bits(self) -> u8 {
        match self {
            Kind::P010 | Kind::I420p10 | Kind::I422p10 => 10,
            Kind::I444p12 => 12,
            Kind::Gray16 => 16,
            _ => 8,
        }
    }

    fn chroma(self) -> Chroma {
        match self {
            Kind::Nv12 | Kind::P010 | Kind::I420 | Kind::I420p10 => Chroma::Yuv420,
            Kind::I422 | Kind::I422p10 => Chroma::Yuv422,
            _ => Chroma::Yuv444,
        }
    }

    pub fn layout(self) -> PixelLayout {
        let bytes = if self.bits() > 8 {
            SampleBytes::Two
        } else {
            SampleBytes::One
        };
        let (sx, sy) = self.chroma().shift();
        let sub = (sx as u8, sy as u8);
        let plane = |components, log2_sub| PlaneDesc {
            components,
            bytes,
            log2_sub,
        };
        let (planes, family) = match self {
            Kind::Nv12 | Kind::P010 | Kind::Nv24 => {
                (vec![plane(1, (0, 0)), plane(2, sub)], ColorFamily::Yuv)
            }
            Kind::Gray | Kind::Gray16 => (vec![plane(1, (0, 0))], ColorFamily::Gray),
            Kind::Gbrp => (vec![plane(1, (0, 0)); 3], ColorFamily::Rgb),
            _ => (
                vec![plane(1, (0, 0)), plane(1, sub), plane(1, sub)],
                ColorFamily::Yuv,
            ),
        };
        PixelLayout {
            name: self.name().into(),
            planes,
            family,
            bit_depth: self.bits(),
            msb_aligned: self == Kind::P010,
        }
    }
}

pub fn color(range: Range, matrix: Matrix, siting: ChromaSiting) -> ColorInfo {
    ColorInfo {
        codes: ColorCodes::UNSPECIFIED,
        range,
        matrix,
        transfer: Transfer::Sdr,
        primaries: Primaries::Bt709,
        siting,
        assumed: Assumed::default(),
    }
}

/// `img` coded as `kind` with `color`'s range and matrix (synth's
/// conversion: chroma averaged over each block, or the left column of a
/// 4:2:2 pair), each row `pad` bytes longer than it needs be.
pub fn frame(img: &RgbImage, kind: Kind, color: ColorInfo, pad: usize) -> Frame {
    let (w, h) = (img.width, img.height);
    let bits = u32::from(kind.bits());
    let codes: Vec<Vec<u16>> = match kind {
        Kind::Gbrp => {
            // G, B and R, each 8-bit, full or limited range as luma.
            let q = |v: f64| -> u16 {
                match color.range {
                    Range::Full => (v.clamp(0.0, 1.0) * 255.0).round() as u16,
                    Range::Limited => (16.0 + 219.0 * v.clamp(0.0, 1.0)).round() as u16,
                }
            };
            [1usize, 2, 0]
                .iter()
                .map(|&c| img.pixels.iter().map(|p| q(p[c])).collect())
                .collect()
        }
        _ => {
            let (kr, kb) = color.matrix.kr_kb().unwrap_or((0.2126, 0.0722));
            let format = YuvFormat {
                kr,
                kb,
                full_range: color.range == Range::Full,
                chroma: kind.chroma(),
            };
            match yuv::to_yuv(img, bits, &format) {
                yuv::YuvPlanes::U8(p) => p
                    .iter()
                    .map(|v| v.iter().map(|&c| u16::from(c)).collect())
                    .collect(),
                yuv::YuvPlanes::U16(p) => p.to_vec(),
            }
        }
    };
    let layout = kind.layout();
    let sizes: Vec<(u32, u32)> = layout.planes.iter().map(|p| p.size(w, h)).collect();
    let samples: Vec<Vec<u16>> = match kind {
        Kind::Nv12 | Kind::P010 | Kind::Nv24 => {
            let uv = codes[1]
                .iter()
                .zip(&codes[2])
                .flat_map(|(&u, &v)| [u, v])
                .collect();
            vec![codes[0].clone(), uv]
        }
        Kind::Gray | Kind::Gray16 => vec![codes[0].clone()],
        _ => codes,
    };
    let shift = if kind == Kind::P010 { 6 } else { 0 };
    let planes = layout
        .planes
        .iter()
        .zip(&sizes)
        .zip(&samples)
        .map(|((desc, &(pw, ph)), s)| {
            let row = desc.row_bytes(pw);
            let stride = row + pad;
            let mut bytes = vec![0u8; stride * (ph as usize - 1) + row];
            let per_row = pw as usize * usize::from(desc.components);
            for y in 0..ph as usize {
                for x in 0..per_row {
                    let c = s[y * per_row + x] << shift;
                    match desc.bytes {
                        SampleBytes::One => bytes[y * stride + x] = c as u8,
                        SampleBytes::Two => {
                            bytes[y * stride + 2 * x..y * stride + 2 * x + 2]
                                .copy_from_slice(&c.to_le_bytes());
                        }
                    }
                }
            }
            (bytes, stride, (pw, ph))
        })
        .collect();
    Frame {
        layout,
        size: (w, h),
        planes,
        color,
    }
}

/// A tiny xorshift generator, so that the tests' noise is the same on
/// every run.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

// ---------------------------------------------------------------------
// The 8-bit mip pass before it rounded its means (DECISIONS D31)

/// What the mip pass returned for component `c` of texel `(i, j)` of the
/// level below `codes` (`size` texels of `comps` components, rows top to
/// bottom) before it rounded to codes itself: the area mean in f32,
/// summed as the shader summed it, each weight an overlap over S and
/// each texel its code / 255.
pub fn unrounded_mip_mean(
    codes: &[u8],
    (sw, sh): (u32, u32),
    comps: usize,
    (i, j): (u32, u32),
    c: usize,
) -> f32 {
    let weights = |j: u32, ss: u32| {
        let ds = (ss / 2).max(1);
        let (lo, hi) = (j * ss, j * ss + ss);
        (lo / ds..=(hi - 1) / ds).map(move |x| {
            (
                x,
                (hi.min((x + 1) * ds) - lo.max(x * ds)) as f32 / ss as f32,
            )
        })
    };
    let mut acc = 0f32;
    for (y, wy) in weights(j, sh) {
        for (x, wx) in weights(i, sw) {
            let code = codes[(y as usize * sw as usize + x as usize) * comps + c];
            acc += wx * wy * (f32::from(code) / 255.0);
        }
    }
    acc
}

/// The code RADV's export stores for a fragment's output `v` into an
/// 8-bit unorm target: `v` packed to f16 toward zero
/// (`v_cvt_pkrtz_f16_f32`), then converted to the nearest code, which
/// puts a value on a half code, or less than a half's step above one, a
/// code low (D31).
pub fn radv_unorm8(v: f32) -> u8 {
    let x = f64::from(v.clamp(0.0, 1.0));
    let e = ((x.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    let step = 2f64.powi(if e < -14 { -24 } else { e - 10 });
    ((x / step).floor() * step * 255.0).round_ties_even() as u8
}

/// The code lavapipe's export stores: the f32 times 255, to the nearest
/// code, ties to even (D31's probe).
pub fn lavapipe_unorm8(v: f32) -> u8 {
    f64::from(v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
}
