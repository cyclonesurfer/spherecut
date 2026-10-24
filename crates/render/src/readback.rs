//! Textures back to the CPU: `render_still`'s pictures, and the levels of
//! a slot for tests. Readback waits for the GPU, so it is for headless
//! work only, never the viewer.

use crate::error::RenderError;
use crate::gpu::Gpu;

/// Texels read back, row after row with no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texels {
    pub width: u32,
    pub height: u32,
    pub bytes_per_texel: u32,
    pub data: Vec<u8>,
}

impl Texels {
    /// The bytes of texel `(x, y)`.
    pub fn texel(&self, x: u32, y: u32) -> Option<&[u8]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let b = self.bytes_per_texel as usize;
        let at = (y as usize * self.width as usize + x as usize) * b;
        self.data.get(at..at + b)
    }

    /// Every 16-bit component as an f32: for `R16Float`, `Rg16Float` and
    /// `Rgba16Float` texels.
    pub fn f16_values(&self) -> Vec<f32> {
        self.data
            .chunks_exact(2)
            .map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]])))
            .collect()
    }
}

/// Level `level` of `texture`, whole.
pub fn read_texture(gpu: &Gpu, texture: &wgpu::Texture, level: u32) -> Result<Texels, RenderError> {
    if level >= texture.mip_level_count() {
        return Err(RenderError::Invalid(format!(
            "level {level} of a texture with {}",
            texture.mip_level_count()
        )));
    }
    let size = (
        (texture.width() >> level).max(1),
        (texture.height() >> level).max(1),
    );
    read_region(gpu, texture, level, size)
}

/// The top-left `size` of level `level` of `texture`. Waits for the GPU.
pub fn read_region(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    level: u32,
    size: (u32, u32),
) -> Result<Texels, RenderError> {
    let format = texture.format();
    let Some(bpt) = format.block_copy_size(None) else {
        return Err(RenderError::Invalid(format!(
            "{format:?} can't be read back"
        )));
    };
    let (w, h) = size;
    let full = (
        (texture.width() >> level).max(1),
        (texture.height() >> level).max(1),
    );
    if w == 0 || h == 0 || w > full.0 || h > full.1 {
        return Err(RenderError::Invalid(format!(
            "{w}x{h} of a {}x{} level",
            full.0, full.1
        )));
    }
    let row = w as usize * bpt as usize;
    let pitch = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let device = &gpu.device;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (pitch * h as usize) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch as u32),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let (tx, rx) = std::sync::mpsc::channel();
    enc.map_buffer_on_submit(&buffer, wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    gpu.queue.submit([enc.finish()]);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(RenderError::Readback(e.to_string()));
    }
    rx.recv()
        .map_err(|_| RenderError::Readback("the copy never finished".into()))?
        .map_err(|e| RenderError::Readback(e.to_string()))?;
    let mut data = Vec::with_capacity(row * h as usize);
    {
        let view = buffer
            .get_mapped_range(..)
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        for y in 0..h as usize {
            data.extend_from_slice(&view[y * pitch..y * pitch + row]);
        }
    }
    buffer.unmap();
    Ok(Texels {
        width: w,
        height: h,
        bytes_per_texel: bpt,
        data,
    })
}

/// An IEEE half as an f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let mant = f32::from(h & 0x3ff);
    match exp {
        0 => sign * mant * (2.0f32).powi(-24),
        31 if mant == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        e => sign * (1.0 + mant / 1024.0) * (2.0f32).powi(e - 15),
    }
}

/// `v` rounded to the nearest IEEE half, ties to even, as an f64: what a
/// value becomes when the GPU stores it in an f16 texture. Past the
/// largest half it is infinite.
pub fn round_f16(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let a = v.abs();
    // The spacing of halves around `a`: 2^(e − 10) for 2^e ≤ a < 2^(e+1),
    // and 2^-24 among the subnormals.
    let e = a.log2().floor().max(-14.0);
    let step = (e - 10.0).exp2();
    let r = (a / step).round_ties_even() * step;
    if r > 65504.0 {
        return f64::INFINITY.copysign(v);
    }
    r.copysign(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_decode() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0001), 2.0f32.powi(-24));
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
    }

    proptest::proptest! {
        /// Every finite half is its own rounding, and rounding moves a
        /// value by at most half the spacing of the halves around it.
        #[test]
        fn halves_round_to_themselves(h in 0u16..=0xffff, v in -70000.0..70000.0f64) {
            let x = f16_to_f32(h);
            if x.is_finite() {
                proptest::prop_assert_eq!(round_f16(f64::from(x)), f64::from(x));
            }
            let r = round_f16(v);
            if r.is_finite() {
                let spacing = (v.abs().log2().floor().max(-14.0) - 10.0).exp2();
                proptest::prop_assert!((r - v).abs() <= spacing / 2.0, "{v} to {r}");
            } else {
                proptest::prop_assert!(v.abs() > 65504.0, "{v}");
            }
        }
    }

    #[test]
    fn rounding_lands_on_halves() {
        assert_eq!(round_f16(1.0), 1.0);
        // 1 + 2^-11 is midway between 1 and 1 + 2^-10: ties to even.
        assert_eq!(round_f16(1.0 + 2f64.powi(-11)), 1.0);
        assert_eq!(
            round_f16(1.0 + 3.0 * 2f64.powi(-11)),
            1.0 + 2.0 * 2f64.powi(-10)
        );
        assert_eq!(round_f16(1.0 / 3.0), f64::from(f16_to_f32(0x3555)));
        assert_eq!(round_f16(-0.1), -f64::from(f16_to_f32(0x2e66)));
        assert_eq!(round_f16(3e-8), 2f64.powi(-24));
        assert_eq!(round_f16(1e6), f64::INFINITY);
        // Every 10-bit code / 1023 is within a quarter of a code.
        for c in 0..1024 {
            let v = f64::from(c) / 1023.0;
            assert!((round_f16(v) - v).abs() <= 0.25 / 1023.0, "{c}");
        }
    }
}
