//! Tier 1, on lavapipe: slice 5b's path B end to end, but for the
//! decoder. FFmpeg's Vulkan context is made on the renderer's own device,
//! a frame is uploaded into one of its images by FFmpeg, the copier
//! copies it out under FFmpeg's frame lock, and the uploader fills a slot
//! from the copy, byte for byte as the copy path fills one from the
//! frame's bytes. FFmpeg then downloads the frame again, after the copy
//! (its next use waits for the copier's signal), and gets the bytes back.
//!
//! lavapipe has one queue, so FFmpeg, the copier and wgpu share it, which
//! `vkshare::single_queue_gpu` allows only one thing at a time from one
//! thread: this binary holds one test.
//!
//! Under the Khronos validation layer (1.3.275 here) it reports none of
//! the copier's or wgpu's work. It does report FFmpeg's own uploads:
//! FFmpeg's frame barriers name a multi-planar image's colour aspect,
//! which the layer tracks apart from the planes its copies name (FFmpeg
//! 6.1 and 8.1 alike, `ff_vk_frame_barrier`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::Kind;
use spherecut_core::Time;
use spherecut_core::video::{ChromaSiting, Matrix, Range, SampleBytes, VideoPlanes};
use spherecut_media::HwDevice;
use spherecut_media::backend::{Transfer, VideoFrame};
use spherecut_render::readback::read_texture;
use spherecut_render::{
    AdapterChoice, Gpu, GpuFrame, Placement, SlotShape, Uploader, upload, vkshare,
};

/// A `kind` frame of `size` with every sample from a hash of its place,
/// rows `pad` bytes longer than they need be.
fn frame(kind: Kind, size: (u32, u32), pad: usize) -> VideoFrame {
    let layout = kind.layout();
    let mut planes = Vec::new();
    let mut strides = Vec::new();
    for (p, desc) in layout.planes.iter().enumerate() {
        let (w, h) = desc.size(size.0, size.1);
        let row = desc.row_bytes(w);
        let stride = row + pad;
        let mut bytes = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..row {
                let n = (p * 7919 + y * 104_729 + x * 31) as u32;
                bytes[y * stride + x] = (n.wrapping_mul(2_654_435_761) >> 24) as u8;
            }
        }
        if desc.bytes == SampleBytes::Two {
            // P010's codes are in the high ten bits; the low six stay as
            // the hash made them, as a decoder would leave them zero, so
            // that a copy that drops or shifts them shows.
            assert!(layout.msb_aligned);
        }
        planes.push(bytes);
        strides.push(stride);
    }
    VideoFrame::owned(
        Arc::new(layout),
        size,
        planes,
        strides,
        common::color(Range::Limited, Matrix::Bt709, ChromaSiting::Left),
        (0, Time::ZERO),
    )
    .unwrap()
}

/// Every level of every plane of `slot`, as `gpu` reads it back.
fn levels(gpu: &Gpu, slot: &GpuFrame) -> Vec<Vec<Vec<u8>>> {
    slot.shape()
        .planes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let t = slot.plane_texture(i).unwrap();
            (0..p.levels)
                .map(|l| read_texture(gpu, t, l).unwrap().data)
                .collect()
        })
        .collect()
}

/// Each plane's bytes, row by row without the padding.
fn rows(frame: &VideoFrame) -> Vec<Vec<u8>> {
    (0..frame.layout().planes.len())
        .map(|i| {
            let p = frame.plane(i).unwrap();
            let row = frame.layout().planes[i].row_bytes(p.width);
            (0..p.height as usize)
                .flat_map(|y| p.bytes[y * p.stride..][..row].to_vec())
                .collect()
        })
        .collect()
}

#[test]
fn ffmpegs_vulkan_frames_fill_a_slot_as_their_bytes_do() {
    // SAFETY: this test is its binary's only one, and does one thing at a
    // time on the device from this thread, waiting for each.
    let gpu = unsafe { vkshare::single_queue_gpu(&AdapterChoice::Cpu) }
        .expect("lavapipe, Mesa's Vulkan CPU driver, is needed for the render tests");
    let shared = gpu.shared_video().unwrap().clone();
    let device =
        HwDevice::vulkan_shared(&shared.handles).expect("FFmpeg's context on wgpu's device");
    assert_eq!(device.kind(), "vulkan");
    let mut uploader = Uploader::new(&gpu);
    let top_bottom = Placement {
        stereo: spherecut_render::Stereo::TopBottom,
        ..Placement::FULL
    };
    for kind in [Kind::Nv12, Kind::P010] {
        for (size, pad) in [((96u32, 48u32), 0usize), ((70, 34), 34)] {
            let frame = frame(kind, size, pad);
            let vk = frame.uploaded_to_vulkan(&device).unwrap();
            assert_eq!(vk.transfer, Transfer::Vulkan);
            assert!(vk.on_device() && vk.plane(0).is_none() && vk.dmabuf().is_none());
            let picture = vk.vulkan().unwrap();
            assert_eq!(picture.planes().len(), 2, "{kind:?}");
            for placement in [Placement::FULL, top_bottom] {
                let what = format!("{kind:?} {size:?} {:?}", placement.stereo);
                let shape =
                    SlotShape::of(&frame.layout_arc().as_ref().clone(), placement.shown(size))
                        .unwrap()
                        .with_levels(4);
                let uploaded = GpuFrame::new(&gpu, &shape).unwrap();
                uploader
                    .upload_placed(&gpu, &frame, &placement, &uploaded)
                    .unwrap();
                let regions = upload::shown_regions(&vk, &placement).unwrap();
                let copied = shared.copier.copy(picture, &regions).unwrap();
                shared.copier.wait(&copied).unwrap();
                shared.copier.order_before(&gpu.queue, &copied);
                let slot = GpuFrame::new(&gpu, &shape).unwrap();
                let stats = uploader
                    .upload_from_buffer(
                        &gpu,
                        &vk,
                        &copied.buffer,
                        &copied.planes,
                        &placement,
                        &slot,
                    )
                    .unwrap();
                assert_eq!(stats.bytes, 0, "{what}");
                assert_eq!(levels(&gpu, &slot), levels(&gpu, &uploaded), "{what}");
            }
            // FFmpeg's own next use of the frame, after two copies.
            let back = vk.to_system().unwrap().unwrap();
            assert_eq!(
                rows(&back),
                rows(&frame),
                "{kind:?} {size:?} downloaded again"
            );
        }
    }
}
