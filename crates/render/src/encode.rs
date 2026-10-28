//! The encode pass (M1 plan section 4.3): a linear target into display
//! values with the sRGB OETF, one texel per pixel, rounded to the output's
//! codes in the shader (DECISIONS D31).

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

/// The largest code of `format`'s colour channels, which the encode pass
/// rounds its output to: 255 for `Rgba8Unorm` and `Bgra8Unorm`, 1023 for
/// `Rgb10a2Unorm`. `None` for any other format, which gets the OETF's
/// value as it is, as a float format should.
pub fn code_max(format: wgpu::TextureFormat) -> Option<u32> {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba8Unorm | F::Bgra8Unorm => Some(255),
        F::Rgb10a2Unorm => Some(1023),
        _ => None,
    }
}

/// The encode pipeline for one output format: egui's `Rgba8Unorm` or
/// `Bgra8Unorm` (slice 7), or the offscreen `Rgba8Unorm` of
/// [`crate::render_still`]. The shader rounds to the format's codes
/// itself ([`code_max`]) and returns k / (2^bits − 1), which every
/// driver's unorm conversion gives back as k, where the OETF's own value
/// came out a code low on RADV near ties. The format is one of those of
/// [`code_max`] or a float format (`Rgba16Float`, `Rgba32Float`), which
/// takes the OETF unrounded. Any other isn't refused, but is wrong: an
/// sRGB format applies the OETF a second time, and a unorm format of
/// another depth is left to the driver's conversion.
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
        // 0 asks the shader not to round.
        let max = code_max(format).map_or(0.0, f64::from);
        let pipeline = layouts::fullscreen_pipeline(
            device,
            "encode",
            &module,
            "fs_encode",
            &[("CODE_MAX", max)],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readback::round_f16;
    use crate::reference::{srgb_eotf, srgb_oetf};

    /// `v ≥ 0` packed to a half and read back: toward zero, as RADV's
    /// `v_cvt_pkrtz_f16_f32` packs a fragment's output for an 8- or
    /// 10-bit unorm target, or to the nearest, ties to even. From the
    /// bits of `v`: halves are 2^(e − 10) apart for 2^e ≤ v < 2^(e+1),
    /// and 2^−24 apart below 2^−14.
    fn half(v: f32, toward_zero: bool) -> f64 {
        let e = ((v.to_bits() >> 23) & 0xff) as i32 - 127;
        let step = 2f64.powi(e.max(-14) - 10);
        let q = f64::from(v) / step;
        let q = if toward_zero {
            q.floor()
        } else {
            q.round_ties_even()
        };
        q * step
    }

    /// The unorm conversion Vulkan asks for, to nearest.
    fn unorm(v: f64, max: u32) -> u32 {
        (v.clamp(0.0, 1.0) * f64::from(max)).round() as u32
    }

    #[test]
    fn every_code_comes_through_the_drivers_export() {
        for (format, max) in [
            (wgpu::TextureFormat::Rgba8Unorm, 255u32),
            (wgpu::TextureFormat::Bgra8Unorm, 255),
            (wgpu::TextureFormat::Rgb10a2Unorm, 1023),
        ] {
            assert_eq!(code_max(format), Some(max), "{format:?}");
        }
        assert_eq!(code_max(wgpu::TextureFormat::Rgba16Float), None);
        for max in [255u32, 1023] {
            let l = max as f32;
            // A pack toward zero moves a value in [0.5, 1) down by less
            // than a half's step there, 2^−11: 0.1245 codes at 8 bits and
            // 0.4995 at 10, so rounding to nearest gives k back.
            let bound = f64::from(max) * 2f64.powi(-11);
            let mut drop = 0f64;
            for k in 0..=max {
                let q = k as f32 / l;
                // Vulkan's conversion must keep an exact integer, and k / L
                // in f32 times L is k in f32 for every k.
                assert_eq!(q * l, k as f32, "{k} / {max}");
                // WGSL's division may be 2.5 ulp out: each neighbour within
                // 3 ulp must come through too.
                for n in -3i64..=3 {
                    let bits = (i64::from(q.to_bits()) + n).max(0) as u32;
                    let f = f32::from_bits(bits).min(1.0);
                    drop = drop.max((f64::from(f) - half(f, true)) * f64::from(max));
                    assert_eq!(unorm(f64::from(f), max), k, "{k} / {max}, unpacked");
                    for (toward_zero, how) in [(true, "toward zero"), (false, "to nearest")] {
                        let got = unorm(half(f, toward_zero), max);
                        assert_eq!(got, k, "{k} / {max}, packed {how}");
                    }
                }
            }
            assert!(drop < bound, "{max}: {drop} codes");
        }
        // The control: unrounded, the OETF's values for flat fields of
        // limited-range luma (passthrough_exact's chain: Y′ = (k − 16) /
        // 219, the EOTF, the f16 target's nearest half, the OETF) come
        // out a code low through a pack toward zero at exactly the ten
        // codes odin's RX 7600 gave off the model (hw-reports/odin-
        // be38fa409e.md); e.g. 129 is 131.5735 codes, packed 131.4844.
        // Full range lies within 0.04 codes of k, and comes through.
        for limited in [false, true] {
            let mut low = Vec::new();
            for k in 0..=255u8 {
                let y = if limited {
                    (f64::from(k) - 16.0) / 219.0
                } else {
                    f64::from(k) / 255.0
                };
                let v = srgb_oetf(round_f16(srgb_eotf(y.clamp(0.0, 1.0))));
                let want = unorm(v, 255);
                if unorm(half(v as f32, true), 255) != want {
                    low.push(k);
                }
                // Rounded first, as the shader now does, it comes through.
                let coded = (f64::from(want) / 255.0) as f32;
                assert_eq!(unorm(half(coded, true), 255), want, "{k}");
            }
            let want: &[u8] = if limited {
                &[74, 129, 135, 141, 153, 165, 208, 214, 220, 226]
            } else {
                &[]
            };
            assert_eq!(low, want, "limited {limited}");
        }
    }
}
