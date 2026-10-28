//! The encode pass (M1 plan section 4.3): a linear target into display
//! values with the sRGB OETF, one texel per pixel, rounded to the output's
//! codes, or to halves, in the shader (DECISIONS D31).

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

/// Whether the encode pass returns its output as the nearest half:
/// for `Rgb10a2Unorm`, whose k / 1023 an export that packs to f16
/// toward zero first (RADV's does for 8-bit targets; nothing has
/// measured its 10-bit ones) can put 0.0005 codes above the tie below k,
/// where the nearest half lies within 0.25 codes of k and every export
/// keeps it as it is; and for `Rgba16Float`, which then stores the half
/// the pass returns, whatever the driver's own conversion does.
pub fn returns_half(format: wgpu::TextureFormat) -> bool {
    use wgpu::TextureFormat as F;
    matches!(format, F::Rgb10a2Unorm | F::Rgba16Float)
}

/// The encode pipeline for one output format: egui's `Rgba8Unorm` or
/// `Bgra8Unorm` (slice 7), or the offscreen `Rgba8Unorm` of
/// [`crate::render_still`]. The shader rounds to the format's codes
/// itself ([`code_max`]) and returns k / (2^bits − 1), at 10 bits its
/// nearest half ([`returns_half`]), which a driver's unorm conversion to
/// nearest gives back as k even after a pack to f16 toward zero (at least
/// 0.375 codes from a tie at 8 bits, 0.25 at 10), where the OETF's own
/// value came out a code low on RADV near ties. The format is one of
/// those of [`code_max`] or a float format: `Rgba16Float`, which takes
/// the OETF's nearest half, or `Rgba32Float`, which takes it as it is.
/// Any other isn't refused, but is wrong: an sRGB format applies the OETF
/// a second time, a unorm format of another depth is left to the
/// driver's conversion, and another f16 format to its rounding.
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
        // 0 asks the shader not to round to codes.
        let max = code_max(format).map_or(0.0, f64::from);
        let half = f64::from(u8::from(returns_half(format)));
        let pipeline = layouts::fullscreen_pipeline(
            device,
            "encode",
            &module,
            "fs_encode",
            &[("CODE_MAX", max), ("HALF", half)],
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
    /// `v_cvt_pkrtz_f16_f32` packs a fragment's output for an 8-bit unorm
    /// target (and may for a 10-bit one), or to the nearest, ties to
    /// even. From the bits of `v`: halves are 2^(e − 10) apart for
    /// 2^e ≤ v < 2^(e+1), and 2^−24 apart below 2^−14.
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
        use wgpu::TextureFormat as F;
        for (format, max, as_half) in [
            (F::Rgba8Unorm, 255u32, false),
            (F::Bgra8Unorm, 255, false),
            (F::Rgb10a2Unorm, 1023, true),
        ] {
            assert_eq!(code_max(format), Some(max), "{format:?}");
            assert_eq!(returns_half(format), as_half, "{format:?}");
        }
        for (format, as_half) in [(F::Rgba16Float, true), (F::Rgba32Float, false)] {
            assert_eq!(code_max(format), None, "{format:?}");
            assert_eq!(returns_half(format), as_half, "{format:?}");
        }
        // What the pass returns for k, unpacked or packed to a half toward
        // zero or to nearest, lies within `bound` codes of k, so a
        // conversion to nearest gives k back with room to spare: k / 255,
        // packed toward zero, less than a half's step in [0.5, 1) below
        // it (2^−11, 0.1245 codes); at 10 bits the half nearest k / 1023,
        // which no pack moves, within half that step (0.2498 codes).
        for (max, as_half, bound) in [(255u32, false, 0.1246), (1023, true, 0.25)] {
            let l = max as f32;
            let (mut far, mut k_over_l) = (0f64, 0f64);
            for k in 0..=max {
                let q = k as f32 / l;
                // k / L in f32 times L is k in f32 for every k.
                assert_eq!(q * l, k as f32, "{k} / {max}");
                // WGSL's division may be 2.5 ulp out: each neighbour within
                // 3 ulp must come through too.
                for n in -3i64..=3 {
                    let bits = (i64::from(q.to_bits()) + n).max(0) as u32;
                    let f = f32::from_bits(bits).min(1.0);
                    let codes = |v: f64| v * f64::from(max) - f64::from(k);
                    k_over_l = k_over_l.max(codes(half(f, true)).abs());
                    let out = if as_half { half(f, false) as f32 } else { f };
                    for (v, how) in [
                        (f64::from(out), "unpacked"),
                        (half(out, true), "packed toward zero"),
                        (half(out, false), "packed to nearest"),
                    ] {
                        far = far.max(codes(v).abs());
                        assert_eq!(unorm(v, max), k, "{k} / {max}, {how}");
                    }
                }
            }
            assert!(far < bound, "{max}: {far} codes from k");
            // k / 1023 itself, packed toward zero, comes within 0.0005
            // codes of the tie below k, which is why the pass returns its
            // half.
            if as_half {
                assert!(k_over_l > 0.499, "{max}: {k_over_l}");
            }
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
