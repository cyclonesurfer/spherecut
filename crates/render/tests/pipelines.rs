//! Tier 1, on lavapipe: every pipeline builds, the uniforms match their
//! WGSL structs, readback handles any size, and sizes past the device's
//! textures are refused (M1 plan section 4.5).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::mem::{offset_of, size_of};

use common::gpu;
use spherecut_render::layouts::{ENCODE_WGSL, INGEST_WGSL, MIPS_WGSL, PROJECT_WGSL};
use spherecut_render::readback::{read_region, read_region_in_bands, read_texture};
use spherecut_render::{
    DebugMode, Encoder, Filter, GpuFrame, IngestUniform, LinearTarget, PlaceUniform, Projector,
    RenderError, SlotShape, SourceUniform, StillOptions, Uploader, ViewParams, ViewUniform,
    render_still,
};
use spherecut_sphere::perspective::{Lens, Projection};

/// Runs `f` inside a validation error scope and fails on any error, so
/// that a bad shader or pipeline is this test's failure with wgpu's
/// message rather than a panic in the device's error handler.
fn validated<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let device = &gpu().device;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out = f();
    if let Some(e) = pollster::block_on(scope.pop()) {
        panic!("{what}: {e}");
    }
    out
}

#[test]
fn every_pipeline_builds() {
    let gpu = gpu();
    validated("projection", || {
        let p = Projector::new(gpu);
        for filter in Filter::ALL {
            for debug in DebugMode::ALL {
                p.prepare(filter, debug);
            }
        }
    });
    // Ingest for both deep formats, and mips for all four.
    validated("uploader", || Uploader::new(gpu));
    // Encode into every format whose codes it rounds to, and the f16 one
    // it rounds to halves.
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgb10a2Unorm,
        wgpu::TextureFormat::Rgba16Float,
    ] {
        validated("encode", || Encoder::new(gpu, format));
    }
    // And each shader on its own through naga, for its messages.
    for (name, src) in [
        ("project", PROJECT_WGSL),
        ("mips", MIPS_WGSL),
        ("ingest", INGEST_WGSL),
        ("encode", ENCODE_WGSL),
    ] {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{name}: {e}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

/// `(member, offset)` of the WGSL struct `name` in `src`, and its size.
fn wgsl_struct(src: &str, name: &str) -> (Vec<(String, u32)>, u32) {
    let module = naga::front::wgsl::parse_str(src).unwrap();
    for (_, ty) in module.types.iter() {
        if ty.name.as_deref() != Some(name) {
            continue;
        }
        if let naga::TypeInner::Struct { members, span } = &ty.inner {
            let m = members
                .iter()
                .map(|m| (m.name.clone().unwrap_or_default(), m.offset))
                .collect();
            return (m, *span);
        }
    }
    panic!("no struct {name}");
}

/// Checks each WGSL member against the Rust field at the same position.
fn same_layout(src: &str, name: &str, rust: &[(&str, usize)], size: usize) {
    let (wgsl, span) = wgsl_struct(src, name);
    assert_eq!(span as usize, size, "{name}'s size");
    assert_eq!(wgsl.len(), rust.len(), "{name}'s members: {wgsl:?}");
    for ((w, off), (r, roff)) in wgsl.iter().zip(rust) {
        assert_eq!(*off as usize, *roff, "{name}.{w} against {r}");
        // Padding is named differently on each side; the rest match.
        if !w.starts_with('_') {
            assert_eq!(w, r, "{name}'s members are in another order");
        }
    }
}

#[test]
fn uniforms_match_their_wgsl_structs() {
    macro_rules! fields {
        ($t:ty: $($f:ident),*) => { &[$((stringify!($f), offset_of!($t, $f))),*] };
    }
    same_layout(
        PROJECT_WGSL,
        "View",
        fields!(ViewUniform: rot0, rot1, rot2, size, k, proj, morph, _pad),
        size_of::<ViewUniform>(),
    );
    same_layout(
        PROJECT_WGSL,
        "Source",
        fields!(SourceUniform: m0, m1, m2, cover, uv_scale, uv_offset, chroma_scale,
            chroma_offset, size0, size1, levels, kind, transfer),
        size_of::<SourceUniform>(),
    );
    // `_pad: [u32; 2]` is WGSL's `_pad0` and `_pad1`.
    let ingest: &[(&str, usize)] = &[
        ("shift", offset_of!(IngestUniform, shift)),
        ("max_code", offset_of!(IngestUniform, max_code)),
        ("_pad", offset_of!(IngestUniform, _pad)),
        ("_pad", offset_of!(IngestUniform, _pad) + 4),
    ];
    same_layout(INGEST_WGSL, "Ingest", ingest, size_of::<IngestUniform>());
    same_layout(
        ENCODE_WGSL,
        "Place",
        fields!(PlaceUniform: origin, extent),
        size_of::<PlaceUniform>(),
    );
}

/// Writes `w`×`h` texels of `format` with a pattern, and reads them back.
fn round_trip(format: wgpu::TextureFormat, w: u32, h: u32) {
    let gpu = gpu();
    let bpt = format.block_copy_size(None).unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("readback test"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let row = (w * bpt) as usize;
    // f16 bit patterns of finite numbers only (no NaN to compare).
    let data: Vec<u8> = (0..row * h as usize)
        .map(|i| (i * 7 % 251) as u8 & 0x7b)
        .collect();
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row as u32),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let back = read_texture(gpu, &texture, 0).unwrap();
    assert_eq!((back.width, back.height, back.bytes_per_texel), (w, h, bpt));
    assert_eq!(back.data, data, "{format:?} {w}x{h}");
    // And in bands of one or two padded rows.
    let banded = read_region_in_bands(gpu, &texture, 0, (w, h), 512).unwrap();
    assert_eq!(banded, back, "{format:?} {w}x{h} in bands");
    // A region of it, from the top left.
    if w > 1 && h > 1 {
        let part = read_region(gpu, &texture, 0, (w - 1, h - 1)).unwrap();
        for y in 0..h - 1 {
            let at = y as usize * row;
            let len = ((w - 1) * bpt) as usize;
            assert_eq!(part.data[y as usize * len..][..len], data[at..at + len]);
        }
    }
}

#[test]
fn readback_handles_odd_sizes() {
    for format in [
        wgpu::TextureFormat::R8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba16Float,
    ] {
        for (w, h) in [(1, 1), (3, 5), (63, 2), (65, 3), (257, 1), (1, 70)] {
            round_trip(format, w, h);
        }
    }
    let gpu = gpu();
    let t = LinearTarget::new(gpu, (4, 4)).unwrap();
    assert_eq!(
        read_texture(gpu, t.texture(), 1).unwrap_err().code(),
        "invalid_input"
    );
    assert_eq!(
        read_region(gpu, t.texture(), 0, (5, 1)).unwrap_err().code(),
        "invalid_input"
    );
}

#[test]
fn readback_passes_wgpus_buffer_limit() {
    // 8193×8192 RGBA8 needs 270 532 608 bytes of padded rows, past the
    // 256 MiB a wgpu buffer may have by default: 3a's first readback, one
    // buffer for the whole picture, failed with gpu_failed at sizes the
    // device's textures take (a 16-bit still of 4097×8192, say).
    let gpu = gpu();
    let (w, h) = (8193u32, 8192u32);
    let pitch = u64::from(w * 4).next_multiple_of(256);
    assert!(pitch * u64::from(h) > gpu.device.limits().max_buffer_size);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("large readback test"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    // Three rows written, at the top, across a band's edge and at the
    // bottom; the rest is the zeros wgpu clears a texture to.
    let row = |y: u32| -> Vec<u8> {
        (0..w * 4)
            .map(|i| ((i * 7 + y * 13) % 251) as u8 + 1)
            .collect()
    };
    let band_rows = (spherecut_render::readback::BAND_BYTES / pitch) as u32;
    let written = [0, band_rows, h - 1];
    for &y in &written {
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &row(y),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: w,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }
    let back = read_texture(gpu, &texture, 0).unwrap();
    assert_eq!(back.data.len(), (w * 4) as usize * h as usize);
    let at = |y: u32| &back.data[(y * w * 4) as usize..][..(w * 4) as usize];
    for &y in &written {
        assert!(at(y) == row(y), "row {y}");
    }
    for y in [1, band_rows - 1, band_rows + 1, h - 2] {
        assert!(at(y).iter().all(|&b| b == 0), "row {y}");
    }
}

#[test]
fn sizes_past_the_device_are_too_large() {
    let gpu = gpu();
    let max = gpu.caps.max_texture_2d;
    assert!(max >= 8192, "{max}");
    let too_large = |e: RenderError| {
        assert_eq!(e.code(), "too_large", "{e}");
        assert!(e.to_string().contains(&format!("{max}x{max}")), "{e}");
    };
    too_large(LinearTarget::new(gpu, (max + 1, 16)).unwrap_err());
    let mut t = LinearTarget::new(gpu, (16, 16)).unwrap();
    too_large(t.resize(gpu, (16, max + 1)).unwrap_err());
    let nv12 = common::Kind::Nv12.layout();
    let shape = SlotShape::of(&nv12, (max + 2, 64)).unwrap();
    too_large(GpuFrame::new(gpu, &shape).unwrap_err());
    let img = spherecut_synth::render_equirect(spherecut_synth::Pattern::Grid, 64, 32, 0, 1);
    let frame = common::frame(
        &img,
        common::Kind::Nv12,
        common::color(
            spherecut_core::video::Range::Limited,
            spherecut_core::video::Matrix::Bt709,
            spherecut_core::video::ChromaSiting::Left,
        ),
        0,
    );
    let view = ViewParams::new(
        glam::DQuat::IDENTITY,
        Lens::new(Projection::Rectilinear, 1.5),
    );
    too_large(render_still(gpu, &frame, &view, (max + 1, 8), StillOptions::default()).unwrap_err());
    let e = render_still(gpu, &frame, &view, (0, 8), StillOptions::default()).unwrap_err();
    assert_eq!(e.code(), "invalid_input");
}
