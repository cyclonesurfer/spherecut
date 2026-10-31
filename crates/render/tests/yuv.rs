//! The YUV pass (M2 plan section 7.3) on lavapipe: against its CPU
//! reference on every 8-bit code and a 10-bit ramp, in each of the four
//! layouts, with the rows' padding zero; and a synthetic equirect
//! rendered at the identity comes back to its own codes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Kind, color, frame, gpu};
use glam::DQuat;
use spherecut_core::video::{ChromaSiting, Matrix, Range};
use spherecut_render::readback::round_f16;
use spherecut_render::yuv::{self, YuvConverter, YuvLayout, YuvShape};
use spherecut_render::{StillOptions, StillRenderer, ViewParams};
use spherecut_synth::RgbImage;

/// The bits of `v`, an IEEE half already.
fn bits_of_half(v: f64) -> u16 {
    if v == 0.0 {
        return 0;
    }
    let e = v.log2().floor() as i32;
    if e >= -14 {
        let m = ((v / 2f64.powi(e) - 1.0) * 1024.0).round() as u16;
        (((e + 15) as u16) << 10) | m
    } else {
        (v * 2f64.powi(24)).round() as u16
    }
}

fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// A `size` linear target holding `rgb` (R, G, B per pixel), each value
/// as its nearest half, and those halves.
fn target(size: (u32, u32), rgb: &[f64]) -> (wgpu::Texture, Vec<f64>) {
    let halves: Vec<f64> = rgb.iter().map(|v| round_f16(*v)).collect();
    let texels: Vec<u16> = halves
        .chunks(3)
        .flat_map(|p| {
            [
                bits_of_half(p[0]),
                bits_of_half(p[1]),
                bits_of_half(p[2]),
                0x3c00,
            ]
        })
        .collect();
    let g = gpu();
    let texture = g.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("yuv test input"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    g.queue.write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&texels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 8),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
    (texture, halves)
}

fn convert(texture: &wgpu::Texture, shape: &YuvShape) -> Vec<u8> {
    let g = gpu();
    let converter = YuvConverter::new(g);
    let out = YuvConverter::buffer(g, shape);
    let mut enc = g
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    converter.record(g, &mut enc, &view, shape, &out).unwrap();
    g.queue.submit([enc.finish()]);
    yuv::read_buffer(g, &out, shape.bytes).unwrap()
}

/// Every sample of `got` against the reference's `codes`: equal, or a
/// code apart where the reference lies within 0.01 codes of a tie, where
/// the GPU's own `pow` decides. Returns how many were near a tie.
fn compare(what: &str, shape: &YuvShape, got: &[u8], codes: &yuv::Codes) -> usize {
    let bits = shape.layout.bits();
    let (w, h) = shape.size;
    let scale = if bits == 10 { 4.0 } else { 1.0 };
    let mut ties = 0;
    let mut wrong = Vec::new();
    let mut each = |p: usize, x: u32, y: u32, v: f64| {
        let want = yuv::to_code(v, bits);
        let have = yuv::sample(shape, got, p, x, y);
        let frac = (v * scale).fract();
        let near = (frac - 0.5).abs() < 0.01;
        ties += usize::from(near);
        if have != want && !(near && have.abs_diff(want) == 1) {
            wrong.push((p, x, y, have, want, v * scale));
        }
    };
    for y in 0..h {
        for x in 0..w {
            each(0, x, y, codes.y[(y * w + x) as usize]);
        }
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let i = (y * w / 2 + x) as usize;
            each(1, x, y, codes.cb[i]);
            each(2, x, y, codes.cr[i]);
        }
    }
    assert!(
        wrong.is_empty(),
        "{what}: {} samples off (plane, x, y, got, want, exact): {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)]
    );
    // The rows' padding, past each row's samples, is zero.
    for (i, p) in shape.planes.iter().enumerate() {
        for row in 0..p.rows {
            let start = (p.offset + u64::from(row) * u64::from(p.stride)) as usize;
            let pad = &got[start + p.row_bytes as usize..start + p.stride as usize];
            assert!(
                pad.iter().all(|b| *b == 0),
                "{what}: plane {i} row {row}'s padding"
            );
        }
    }
    ties
}

/// 64 × 32 pixels: each channel through every 8-bit code, in three
/// orders, so that neighbours differ.
fn every_code() -> Vec<f64> {
    (0..64 * 32u32)
        .flat_map(|n| {
            [n % 256, (n * 7 + 3) % 256, (n * 13 + 101) % 256]
                .map(|k| srgb_eotf(f64::from(k) / 255.0))
        })
        .collect()
}

/// 64 × 32 pixels: a grey ramp through all 1024 10-bit codes, then a
/// coloured one.
fn ten_bit_ramp() -> Vec<f64> {
    (0..64 * 32u32)
        .flat_map(|n| {
            let k = n % 1024;
            let c = if n < 1024 {
                [k, k, k]
            } else {
                [k, 1023 - k, (k * 5) % 1024]
            };
            c.map(|k| srgb_eotf(f64::from(k) / 1023.0))
        })
        .collect()
}

#[test]
fn the_pass_matches_its_reference_in_every_layout() {
    let size = (64, 32);
    for (input, rgb) in [
        ("every 8-bit code", every_code()),
        ("a 10-bit ramp", ten_bit_ramp()),
    ] {
        let (texture, halves) = target(size, &rgb);
        let codes = yuv::reference(&halves, size);
        for layout in YuvLayout::ALL {
            let shape = YuvShape::new(layout, size).unwrap();
            let got = convert(&texture, &shape);
            let what = format!("{input}, {}", layout.name());
            let ties = compare(&what, &shape, &got, &codes);
            eprintln!("{what}: exact, {ties} samples near a tie");
            // About 2 % of samples sit within 0.01 codes of a tie, as
            // chance has it; many more would mean the margin hides a
            // rounding fault.
            let samples = (size.0 * size.1 * 3 / 2) as usize;
            assert!(
                ties * 20 < samples,
                "{what}: {ties} of {samples} near a tie"
            );
        }
    }
}

/// The P010 and I010 of one input hold the same codes, at the top and
/// at the bottom of their `u16`s; NV12 and I420 the same bytes.
#[test]
fn the_layouts_hold_the_same_codes() {
    let size = (64, 32);
    let (texture, _) = target(size, &every_code());
    let read = |l| {
        let shape = YuvShape::new(l, size).unwrap();
        let data = convert(&texture, &shape);
        (shape, data)
    };
    for (a, b) in [
        (YuvLayout::Nv12, YuvLayout::I420),
        (YuvLayout::P010, YuvLayout::I010),
    ] {
        let ((sa, da), (sb, db)) = (read(a), read(b));
        for p in 0..3 {
            let (w, h) = if p == 0 {
                size
            } else {
                (size.0 / 2, size.1 / 2)
            };
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        yuv::sample(&sa, &da, p, x, y),
                        yuv::sample(&sb, &db, p, x, y),
                        "{a:?} and {b:?}, plane {p} at ({x}, {y})"
                    );
                }
            }
        }
    }
    // P010's low 6 bits are zero, I010's top 6.
    let (s, d) = read(YuvLayout::P010);
    let (t, e) = read(YuvLayout::I010);
    for y in 0..size.1 {
        for pair in s.row(&d, 0, y).chunks(2) {
            assert_eq!(u16::from_le_bytes([pair[0], pair[1]]) & 0x3f, 0);
        }
        for pair in t.row(&e, 0, y).chunks(2) {
            assert_eq!(u16::from_le_bytes([pair[0], pair[1]]) >> 10, 0);
        }
    }
}

/// NV12 codes for a 256 × 128 equirect, BT.709 limited, left-sited:
/// blocks of 8 × 8 pixels, each one colour's chroma, its luma varying
/// within the block, every pixel inside the RGB cube.
fn blocks() -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (256usize, 128usize);
    let (mut y, mut uv) = (vec![0u8; w * h], vec![0u8; w * h / 2]);
    for by in 0..h / 8 {
        for bx in 0..w / 8 {
            let n = by * 32 + bx;
            // A colour in the middle of the cube, so that ±0.04 of luma
            // keeps R′, G′ and B′ in [0, 1].
            let rgb =
                [n * 37 % 100, n * 61 % 100, n * 83 % 100].map(|k| 0.25 + 0.5 * k as f64 / 100.0);
            let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            let cb = (rgb[2] - luma) / 1.8556;
            let cr = (rgb[0] - luma) / 1.5748;
            for j in 0..8 {
                for i in 0..8 {
                    let l = luma + 0.04 * (((i * 3 + j * 5) % 7) as f64 / 3.0 - 1.0);
                    y[(by * 8 + j) * w + bx * 8 + i] = (16.0 + 219.0 * l).round() as u8;
                }
            }
            for j in 0..4 {
                for i in 0..4 {
                    let at = (by * 4 + j) * w + (bx * 4 + i) * 2;
                    uv[at] = (128.0 + 224.0 * cb).round() as u8;
                    uv[at + 1] = (128.0 + 224.0 * cr).round() as u8;
                }
            }
        }
    }
    (y, uv)
}

/// Section 7.3's round trip: an equirect rendered as equirect at the
/// identity, at its own size, through the EOTF, f16 and the OETF, comes
/// back within a code of its own luma everywhere, and of its own chroma
/// wherever the chroma is the same over what the filters reach (D31's
/// tolerance).
#[test]
fn an_identity_render_comes_back_to_its_codes() {
    let (w, h) = (256u32, 128u32);
    let limited709 = color(Range::Limited, Matrix::Bt709, ChromaSiting::Left);
    let blank = RgbImage {
        width: w,
        height: h,
        pixels: vec![[0.5; 3]; (w * h) as usize],
    };
    let mut f = frame(&blank, Kind::Nv12, limited709, 0);
    let (y, uv) = blocks();
    // Both planes' rows are `w` bytes: luma, and half as many CbCr pairs.
    for ((bytes, stride, (_, rows)), codes) in f.planes.iter_mut().zip([&y, &uv]) {
        let row = w as usize;
        for r in 0..*rows as usize {
            bytes[r * *stride..r * *stride + row].copy_from_slice(&codes[r * row..(r + 1) * row]);
        }
    }
    let mut renderer = StillRenderer::new(gpu());
    for layout in YuvLayout::ALL {
        let got = renderer
            .render_yuv(
                gpu(),
                &f,
                &[ViewParams::equirect(DQuat::IDENTITY)],
                (w, h),
                &StillOptions::default(),
                layout,
            )
            .unwrap();
        let k = if layout.bits() == 10 { 4 } else { 1 };
        let mut worst = [0u32; 3];
        let mut checked = 0;
        for yy in 0..h {
            for x in 0..w {
                let want = u32::from(y[(yy * w + x) as usize]) * k;
                worst[0] = worst[0].max(got.sample(0, x, yy).abs_diff(want));
            }
        }
        for cy in 0..h / 2 {
            for cx in 0..w / 2 {
                // Chroma samples 1 and 2 of each block's 4: their taps,
                // and the rows the source's chroma is rebuilt from, stay
                // inside the block.
                if !(1..=2).contains(&(cx % 4)) || !(1..=2).contains(&(cy % 4)) {
                    continue;
                }
                checked += 1;
                for p in 1..3 {
                    let want = u32::from(uv[(cy * w + cx * 2) as usize + p - 1]) * k;
                    worst[p] = worst[p].max(got.sample(p, cx, cy).abs_diff(want));
                }
            }
        }
        eprintln!(
            "{}: luma within {}, Cb within {}, Cr within {} codes ({checked} chroma samples)",
            layout.name(),
            worst[0],
            worst[1],
            worst[2]
        );
        // D31's tolerance, a code at 8 bits (four at 10); measured exact
        // in all four layouts.
        assert!(
            worst.iter().all(|d| *d <= k),
            "{}: {worst:?}",
            layout.name()
        );
    }
}
