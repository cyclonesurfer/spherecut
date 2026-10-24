//! Tier 1, on lavapipe: a device that is lost (a driver reset, the GPU
//! removed, here `Device::destroy`) makes every entry point return
//! `gpu_failed`, and nothing panics in wgpu's error handler; an error no
//! error scope caught is returned by the next check. Each test has a
//! device of its own, since the binary's tests share nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Kind, color, frame};
use spherecut_core::video::{ChromaSiting, Matrix, Range};
use spherecut_render::readback::read_texture;
use spherecut_render::{
    AdapterChoice, DebugMode, Filter, Gpu, GpuFrame, LinearTarget, Projector, RenderError,
    SlotShape, StillOptions, StillRenderer, Uploader, ViewParams, render_still,
};
use spherecut_sphere::perspective::{Lens, Projection};

#[test]
fn a_lost_device_is_an_error_not_a_panic() {
    let gpu = Gpu::headless(&AdapterChoice::Cpu).unwrap();
    let img = spherecut_synth::render_equirect(spherecut_synth::Pattern::Grid, 64, 32, 0, 1);
    let f = frame(
        &img,
        Kind::P010,
        color(Range::Limited, Matrix::Bt709, ChromaSiting::Left),
        0,
    );
    let shape = SlotShape::of(&f.layout, f.size).unwrap();
    let view = ViewParams::new(glam::DQuat::IDENTITY, Lens::new(Projection::Fisheye, 3.0));
    let slot = GpuFrame::new(&gpu, &shape).unwrap();
    let mut uploader = Uploader::new(&gpu);
    uploader.upload(&gpu, &f, &slot).unwrap();
    let projector = Projector::new(&gpu);
    let target = LinearTarget::new(&gpu, (16, 16)).unwrap();
    let mut still = StillRenderer::new(&gpu);
    still
        .render(&gpu, &f, &view, (16, 16), &StillOptions::default())
        .unwrap();
    assert!(gpu.check().is_ok());

    gpu.device.destroy();
    let lost = |what: &str, e: RenderError| {
        assert_eq!(e.code(), "gpu_failed", "{what}: {e}");
        assert!(e.to_string().contains("lost"), "{what}: {e}");
    };
    lost("check", gpu.check().unwrap_err());
    assert!(gpu.is_lost());
    // What was made before the loss, and what is made after.
    lost("upload", uploader.upload(&gpu, &f, &slot).unwrap_err());
    assert_eq!(uploader.poll(&gpu), uploader.poll(&gpu));
    lost("a new slot", GpuFrame::new(&gpu, &shape).unwrap_err());
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    lost(
        "a projection",
        projector
            .record(
                &mut enc,
                &slot,
                &view,
                &target,
                Filter::Isotropic,
                DebugMode::Off,
                None,
            )
            .unwrap_err(),
    );
    drop(enc);
    lost(
        "a new target",
        LinearTarget::new(&gpu, (16, 16)).unwrap_err(),
    );
    lost(
        "a still",
        still
            .render(&gpu, &f, &view, (16, 16), &StillOptions::default())
            .unwrap_err(),
    );
    lost(
        "a still from scratch",
        render_still(&gpu, &f, &view, (16, 16), StillOptions::default()).unwrap_err(),
    );
    lost(
        "a readback",
        read_texture(&gpu, target.texture(), 0).unwrap_err(),
    );
    // An uploader made after the loss.
    let mut late = Uploader::new(&gpu);
    lost("a late upload", late.upload(&gpu, &f, &slot).unwrap_err());
}

#[test]
fn an_uncaught_error_is_returned_by_the_next_check() {
    let gpu = Gpu::headless(&AdapterChoice::Cpu).unwrap();
    // A buffer mapped at creation whose size isn't a multiple of 4: a
    // validation error, with no error scope to catch it.
    let _bad = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("unaligned"),
        size: 3,
        usage: wgpu::BufferUsages::MAP_WRITE,
        mapped_at_creation: true,
    });
    let e = gpu.check().unwrap_err();
    assert_eq!(e.code(), "gpu_failed", "{e}");
    assert!(e.to_string().contains("no error scope caught"), "{e}");
    assert!(e.to_string().contains("unaligned"), "{e}");
    // Once, and the device isn't lost for it.
    assert!(gpu.check().is_ok());
    assert!(!gpu.is_lost());
}
