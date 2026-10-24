//! Tier 1, on lavapipe: planes into slots (M1 plan sections 4.2 and 4.4).
//! Level 0 holds each plane's codes, deep ones through the ingest pass
//! with P010's shift; each mip level is the area-weighted mean of the one
//! above at odd sizes; both upload paths fill a slot alike; bad frames
//! and wrong slots are refused; a frame already on the GPU fills a slot
//! as an upload does; and the upload bench runs, small (its Tier 2 form,
//! `bench_upload_5k7`, is here too).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Frame, Kind, Rng, gpu};
use spherecut_core::video::{ChromaSiting, Matrix, Range, SampleBytes};
use spherecut_render::readback::{f16_to_f32, read_texture, round_f16};
use spherecut_render::{
    DebugMode, Filter, Gpu, GpuFrame, GpuTimers, LinearTarget, Placement, PlaneFormat, Projector,
    SlotShape, UploadPath, Uploader, ViewParams,
};
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_testkit::bench;

/// A `kind` frame of `size` with every sample random, and `pad` bytes
/// after each row. Deep samples get random bits below their depth too
/// where the format leaves them unused (P010's low six), and one code past
/// the depth where the format puts its bits low, to check the clamp.
fn noise(kind: Kind, size: (u32, u32), pad: usize, seed: u64) -> Frame {
    let mut rng = Rng(seed);
    let layout = kind.layout();
    let bits = u32::from(kind.bits());
    let planes = layout
        .planes
        .iter()
        .map(|desc| {
            let (w, h) = desc.size(size.0, size.1);
            let row = desc.row_bytes(w);
            let stride = row + pad;
            let mut bytes = vec![0u8; stride * (h as usize - 1) + row];
            for y in 0..h as usize {
                for x in 0..w as usize * usize::from(desc.components) {
                    let r = rng.next();
                    match desc.bytes {
                        SampleBytes::One => bytes[y * stride + x] = r as u8,
                        SampleBytes::Two => {
                            let c = if layout.msb_aligned {
                                r as u16
                            } else if x == 1 && y == 1 {
                                u16::MAX
                            } else {
                                (r % (1 << bits)) as u16
                            };
                            bytes[y * stride + 2 * x..][..2].copy_from_slice(&c.to_le_bytes());
                        }
                    }
                }
            }
            (bytes, stride, (w, h))
        })
        .collect();
    Frame {
        layout,
        size,
        planes,
        color: common::color(Range::Limited, Matrix::Bt709, ChromaSiting::Left),
    }
}

/// A `kind` frame of `size` whose sample `n` of each plane is
/// `code(n)`, placed where the format puts its bits (P010's high).
fn coded(kind: Kind, size: (u32, u32), code: impl Fn(usize) -> u16) -> Frame {
    let layout = kind.layout();
    let shift = if layout.msb_aligned {
        16 - layout.bit_depth
    } else {
        0
    };
    let planes = layout
        .planes
        .iter()
        .map(|desc| {
            let (w, h) = desc.size(size.0, size.1);
            let n = w as usize * h as usize * usize::from(desc.components);
            let bytes = (0..n)
                .flat_map(|i| (code(i) << shift).to_le_bytes())
                .collect();
            (bytes, desc.row_bytes(w), (w, h))
        })
        .collect();
    Frame {
        layout,
        size,
        planes,
        color: common::color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
    }
}

/// Every level of every plane of `slot`, as f32 values.
fn levels(slot: &GpuFrame) -> Vec<Vec<(u32, u32, Vec<f32>)>> {
    let gpu = gpu();
    slot.shape()
        .planes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let t = slot.plane_texture(i).unwrap();
            (0..p.levels)
                .map(|l| {
                    let texels = read_texture(gpu, t, l).unwrap();
                    let values = if p.format.is_deep() {
                        texels.f16_values()
                    } else {
                        texels.data.iter().map(|&b| f32::from(b) / 255.0).collect()
                    };
                    (texels.width, texels.height, values)
                })
                .collect()
        })
        .collect()
}

fn slot_for(frame: &Frame, levels: u32) -> GpuFrame {
    let shape = SlotShape::of(&frame.layout, frame.size)
        .unwrap()
        .with_levels(levels);
    GpuFrame::new(gpu(), &shape).unwrap()
}

/// What level 0 of plane `i` should hold: codes / (2^b − 1), after
/// P010's shift and the clamp, in f16 for deep planes.
fn level0(frame: &Frame, i: usize) -> Vec<f64> {
    let (bytes, stride, (w, h)) = &frame.planes[i];
    let comps = usize::from(frame.layout.planes[i].components);
    let deep = frame.layout.bit_depth > 8;
    let max = f64::from((1u32 << frame.layout.bit_depth) - 1);
    let shift = if frame.layout.msb_aligned {
        16 - frame.layout.bit_depth
    } else {
        0
    };
    let mut out = Vec::new();
    for y in 0..*h as usize {
        for x in 0..*w as usize * comps {
            out.push(if deep {
                let c =
                    u16::from_le_bytes([bytes[y * stride + 2 * x], bytes[y * stride + 2 * x + 1]]);
                round_f16((f64::from(c >> shift)).min(max) / max)
            } else {
                f64::from(bytes[y * stride + x]) / 255.0
            });
        }
    }
    out
}

/// The area-weighted mean of `above` (`sw` × `sh`, `comps` per texel)
/// over destination texel `(i, j)` of a `dw` × `dh` level, by coverage of
/// real intervals: destination texel i spans [i·sw/dw, (i + 1)·sw/dw) of
/// the source.
fn area_mean(
    above: &[f32],
    (sw, sh): (u32, u32),
    comps: usize,
    (dw, dh): (u32, u32),
    (i, j): (u32, u32),
    c: usize,
) -> f64 {
    let span = |k: u32, s: u32, d: u32| {
        let (lo, hi) = (
            f64::from(k) * f64::from(s) / f64::from(d),
            f64::from(k + 1) * f64::from(s) / f64::from(d),
        );
        (0..s)
            .filter_map(move |t| {
                let o = (f64::from(t + 1)).min(hi) - f64::from(t).max(lo);
                (o > 0.0).then_some((t, o / (hi - lo)))
            })
            .collect::<Vec<_>>()
    };
    let mut acc = 0.0;
    for (y, wy) in span(j, sh, dh) {
        for (x, wx) in span(i, sw, dw) {
            acc += wx * wy * f64::from(above[(y as usize * sw as usize + x as usize) * comps + c]);
        }
    }
    acc
}

#[test]
fn mips_are_area_means_at_odd_sizes() {
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    // R8 and Rg8 (NV12), R16F and Rg16F (P010), and planar R16F with its
    // bits low (yuv420p10), at sizes that are odd all the way down.
    for (kind, size) in [
        (Kind::Nv12, (45, 23)),
        (Kind::P010, (45, 23)),
        (Kind::I420p10, (37, 29)),
    ] {
        let frame = noise(kind, size, 12, 7);
        let slot = slot_for(&frame, 8);
        uploader.upload(gpu, &frame, &slot).unwrap();
        let got = levels(&slot);
        for (i, plane) in got.iter().enumerate() {
            let p = slot.shape().planes[i];
            let comps = p.format.components() as usize;
            assert_eq!(plane.len() as u32, p.levels);
            assert!(p.levels >= 5, "{kind:?} plane {i}: {} levels", p.levels);
            // Level 0: the codes, exactly for 8 bits (f32 against f64),
            // and the nearest half of each for deep ones.
            let want = level0(&frame, i);
            for (k, (&g, &w)) in plane[0].2.iter().zip(&want).enumerate() {
                let tol = if p.format.is_deep() { 0.0 } else { 1e-7 };
                assert!(
                    (f64::from(g) - w).abs() <= tol,
                    "{kind:?} plane {i} texel {k}: {g} for {w}"
                );
            }
            for l in 1..plane.len() {
                let (sw, sh, above) = &plane[l - 1];
                let (dw, dh, this) = &plane[l];
                assert_eq!((*dw, *dh), ((sw / 2).max(1), (sh / 2).max(1)));
                for j in 0..*dh {
                    for x in 0..*dw {
                        for c in 0..comps {
                            let want = area_mean(above, (*sw, *sh), comps, (*dw, *dh), (x, j), c);
                            let g = f64::from(
                                this[(j as usize * *dw as usize + x as usize) * comps + c],
                            );
                            // One code at 8 bits. For deep planes the
                            // nearest half of the mean, unless the mean is
                            // so near a tie that the shader's f32 sum
                            // could fall on either side of it.
                            if p.format.is_deep() {
                                assert!(
                                    g == round_f16(want) || near_a_tie(want),
                                    "{kind:?} plane {i} level {l} ({x}, {j}) c{c}: {g} for {want}, \
                                     whose nearest half is {}",
                                    round_f16(want)
                                );
                            } else {
                                assert!(
                                    (g - want).abs() <= 1.0 / 255.0,
                                    "{kind:?} plane {i} level {l} ({x}, {j}) c{c}: {g} for {want}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn deep_codes_are_shifted_and_clamped() {
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    // P010: 0x3ff << 6 with the low bits set is still 1023.
    let mut p010 = noise(Kind::P010, (8, 4), 0, 3);
    p010.planes[0].0[..2].copy_from_slice(&0xffffu16.to_le_bytes());
    p010.planes[0].0[2..4].copy_from_slice(&(512u16 << 6 | 0x2a).to_le_bytes());
    let slot = slot_for(&p010, 1);
    uploader.upload(gpu, &p010, &slot).unwrap();
    let l0 = &levels(&slot)[0][0].2;
    assert_eq!(l0[0], 1.0);
    assert_eq!(f64::from(l0[1]), round_f16(512.0 / 1023.0));
    // yuv420p10: 0xffff is past 1023, so it clamps to 1.
    let i010 = noise(Kind::I420p10, (8, 4), 0, 3);
    let slot = slot_for(&i010, 1);
    uploader.upload(gpu, &i010, &slot).unwrap();
    let l0 = &levels(&slot)[0][0].2;
    assert_eq!(l0[8 + 1], 1.0, "the stray code at (1, 1)");
    assert_eq!(f16_to_f32(0x3c00), 1.0);
}

#[test]
fn deep_codes_round_to_the_nearest_half() {
    // Every 10-bit code, high-aligned (P010) and low (yuv420p10), and
    // every 12-bit code, through the ingest pass: each texel must be
    // code / (2^b − 1) rounded to the nearest half, ties to even. The
    // shader rounds it itself; lavapipe's own conversion into an f16
    // target truncates, which leaves about half the codes one half
    // below (DECISIONS D31).
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    for (kind, size) in [
        (Kind::P010, (64, 16)),
        (Kind::I420p10, (64, 16)),
        (Kind::I444p12, (64, 64)),
    ] {
        let max = (1usize << kind.bits()) - 1;
        let frame = coded(kind, size, |n| (n % (max + 1)) as u16);
        let slot = slot_for(&frame, 1);
        uploader.upload(gpu, &frame, &slot).unwrap();
        for (i, plane) in levels(&slot).iter().enumerate() {
            let got = &plane[0].2;
            let wrong: Vec<(usize, f32, f64)> = got
                .iter()
                .enumerate()
                .map(|(n, &g)| (n, g, round_f16((n % (max + 1)) as f64 / max as f64)))
                .filter(|&(_, g, want)| f64::from(g) != want)
                .collect();
            assert!(
                wrong.is_empty(),
                "{kind:?} plane {i}: {} of {} codes aren't their nearest half, such as {:?}",
                wrong.len(),
                got.len(),
                &wrong[..wrong.len().min(4)]
            );
        }
    }
}

/// Whether `v` is so near a tie between two halves that an f32 sum of it
/// could round either way.
fn near_a_tie(v: f64) -> bool {
    round_f16(v * (1.0 - 1e-6)) != round_f16(v * (1.0 + 1e-6))
}

#[test]
fn both_upload_paths_fill_a_slot_alike() {
    let gpu = gpu();
    for kind in [Kind::Nv12, Kind::P010, Kind::I422p10, Kind::Gbrp] {
        // 70 wide with 34 bytes of padding: no plane's rows are 256-byte
        // aligned, so the belt copies them one by one. 256 wide without
        // padding: every plane's rows are a multiple of 256 bytes, the
        // belt's own pitch, so it copies each plane at once.
        for (width, pad) in [(70u32, 34usize), (256, 0)] {
            let frame = noise(kind, (width, 33), pad, 11);
            let aligned = frame.planes.iter().all(|(_, stride, _)| stride % 256 == 0);
            assert_eq!(aligned, pad == 0, "{kind:?} {width}");
            let mut out = Vec::new();
            for path in [UploadPath::WriteTexture, UploadPath::StagingBelt] {
                let mut uploader = Uploader::with_path(gpu, path);
                assert_eq!(uploader.path(), path);
                let slot = slot_for(&frame, 4);
                let stats = uploader.upload(gpu, &frame, &slot).unwrap();
                let bytes: usize = frame
                    .layout
                    .planes
                    .iter()
                    .zip(&frame.planes)
                    .map(|(d, (_, _, (w, h)))| d.row_bytes(*w) * *h as usize)
                    .sum();
                assert_eq!(stats.bytes, bytes as u64);
                out.push(levels(&slot));
            }
            assert_eq!(out[0], out[1], "{kind:?}, {width} wide, pad {pad}");
        }
    }
}

/// What the projection makes of `slot`, read back from the linear target.
fn project(slot: &GpuFrame) -> Vec<u8> {
    let gpu = gpu();
    let projector = Projector::new(gpu);
    let target = LinearTarget::new(gpu, (48, 32)).unwrap();
    let view = ViewParams::new(glam::DQuat::IDENTITY, Lens::new(Projection::Fisheye, 3.0));
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    projector
        .record(
            &mut enc,
            slot,
            &view,
            &target,
            Filter::Isotropic,
            DebugMode::Off,
            None,
        )
        .unwrap();
    gpu.queue.submit([enc.finish()]);
    read_texture(gpu, target.texture(), 0).unwrap().data
}

#[test]
fn a_frame_already_on_the_gpu_fills_a_slot() {
    // As slice 5b's import will: level 0 written on the GPU (here copied
    // from a slot the uploader filled), then the mips and the uniform.
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    for kind in [Kind::Nv12, Kind::P010] {
        let mut frame = noise(kind, (96, 48), 0, 21);
        frame.color = common::color(Range::Full, Matrix::Bt601, ChromaSiting::Center);
        let uploaded = slot_for(&frame, 4);
        uploader.upload(gpu, &frame, &uploaded).unwrap();
        let on_gpu = slot_for(&frame, 4);
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for (i, p) in on_gpu.shape().planes.iter().enumerate() {
            enc.copy_texture_to_texture(
                uploaded.plane_texture(i).unwrap().as_image_copy(),
                on_gpu.plane_texture(i).unwrap().as_image_copy(),
                wgpu::Extent3d {
                    width: p.size.0,
                    height: p.size.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        uploader.record_mips(&mut enc, &on_gpu);
        on_gpu.set_source(gpu, frame.layout.bit_depth, &frame.color, &Placement::FULL);
        gpu.queue.submit([enc.finish()]);
        assert_eq!(levels(&on_gpu), levels(&uploaded), "{kind:?}");
        assert_eq!(project(&on_gpu), project(&uploaded), "{kind:?}");
    }
}

#[test]
fn bad_frames_and_wrong_slots_are_refused() {
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    let frame = noise(Kind::Nv12, (64, 32), 0, 5);
    let other = slot_for(&noise(Kind::Nv12, (64, 34), 0, 5), 1);
    let e = uploader.upload(gpu, &frame, &other).unwrap_err();
    assert_eq!(e.code(), "internal", "{e}");
    let other = slot_for(&noise(Kind::P010, (64, 32), 0, 5), 1);
    assert_eq!(
        uploader.upload(gpu, &frame, &other).unwrap_err().code(),
        "internal"
    );
    // A stereo frame is uploaded as its shown eye, into a slot of that
    // size, not the whole frame's.
    let whole = slot_for(&frame, 1);
    let top_bottom = Placement {
        stereo: spherecut_render::Stereo::TopBottom,
        ..Placement::FULL
    };
    assert_eq!(
        uploader
            .upload_placed(gpu, &frame, &top_bottom, &whole)
            .unwrap_err()
            .code(),
        "internal"
    );
    let eye = GpuFrame::new(
        gpu,
        &SlotShape::of(&frame.layout, top_bottom.shown(frame.size)).unwrap(),
    )
    .unwrap();
    let stats = uploader
        .upload_placed(gpu, &frame, &top_bottom, &eye)
        .unwrap();
    assert_eq!(stats.bytes, 64 * 16 + 64 * 8);
    // A plane one byte short, and one whose rows overlap.
    let slot = slot_for(&frame, 1);
    let mut short = frame.clone();
    short.planes[1].0.pop();
    assert_eq!(
        uploader.upload(gpu, &short, &slot).unwrap_err().code(),
        "invalid_input"
    );
    let mut overlapping = frame.clone();
    overlapping.planes[0].1 = 63;
    assert_eq!(
        uploader
            .upload(gpu, &overlapping, &slot)
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    let mut missing = frame.clone();
    missing.planes.pop();
    assert_eq!(
        uploader.upload(gpu, &missing, &slot).unwrap_err().code(),
        "invalid_input"
    );
    // And the slot still takes a good frame.
    uploader.upload(gpu, &frame, &slot).unwrap();
    // A refused frame leaves the slot as it was, by either path: every
    // plane is checked before any is written.
    for path in [UploadPath::WriteTexture, UploadPath::StagingBelt] {
        let mut uploader = Uploader::with_path(gpu, path);
        uploader.upload(gpu, &frame, &slot).unwrap();
        let before = levels(&slot);
        let mut short = noise(Kind::Nv12, (64, 32), 0, 6);
        short.planes[1].0.pop();
        assert_eq!(
            uploader.upload(gpu, &short, &slot).unwrap_err().code(),
            "invalid_input"
        );
        assert!(
            levels(&slot) == before,
            "{path:?}: a refused frame changed the slot"
        );
    }
}

#[test]
fn the_gpu_time_covers_the_ingest() {
    // A deep frame into a slot of one level: no mips, so the GPU time is
    // the ingest passes', which take most of an upload and its wait on
    // lavapipe. A span that missed them (as 3a's first one did) timed
    // nothing but its own two timestamps: 0.04 ms of 30.
    let gpu = gpu();
    assert!(
        gpu.caps.timestamps_in_encoders,
        "lavapipe has timestamps between passes"
    );
    let mut uploader = Uploader::new(gpu);
    let frame = noise(Kind::P010, (2048, 1024), 0, 13);
    let slot = slot_for(&frame, 1);
    let mut runs = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        uploader.upload(gpu, &frame, &slot).unwrap();
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let wall = start.elapsed().as_secs_f64() * 1e3;
        let timed = f64::from(uploader.poll(gpu).expect("a GPU time once idle"));
        eprintln!("GPU {timed:.3} ms of {wall:.3} ms");
        runs.push(timed / wall);
    }
    // The best run, since the other tests' work on the device can only
    // lengthen the wait (measured 0.8 here).
    let best = runs.iter().copied().fold(0.0, f64::max);
    assert!(
        best > 0.25,
        "the GPU time is at most {best:.4} of the upload's wall time"
    );
}

/// The sizes and counts `bench_upload_5k7` runs at: the plan's for Tier 2,
/// small ones for the Tier 1 smoke run, which only checks that it runs.
struct BenchPlan {
    /// The single frames (5.7K) and each frame of a pair (the X5's 8K is
    /// two 3840² streams).
    single: (u32, u32),
    pair: (u32, u32),
    /// The projection's outputs.
    outputs: [(u32, u32); 2],
    /// Upload steps per run, and projections or mip chains per run.
    frames: usize,
    passes: usize,
    /// Measured runs, after one to warm up (D20).
    runs: usize,
}

/// Waits for the GPU to finish what was submitted.
fn wait(gpu: &Gpu) {
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
}

/// The allocator's block count and reserved bytes, when it reports.
fn allocator(gpu: &Gpu) -> Option<(usize, u64)> {
    gpu.device
        .generate_allocator_report()
        .map(|r| (r.blocks.len(), r.total_reserved_bytes))
}

/// What one run of upload steps cost, per step.
#[derive(Debug, Default)]
struct UploadRun {
    cpu_ms: f64,
    /// The mean of the spans read back: each step's ingest and mips, or
    /// the second frame's of a pair (the two are the same size).
    gpu_ms: Option<f64>,
    /// Steps after which the allocator had another number of blocks than
    /// after the step before, and the fewest and most blocks seen.
    block_changes: usize,
    blocks: Option<(usize, usize)>,
    reserved_max: u64,
}

/// `frames` steps, each uploading every frame of `input` into its slot as
/// the upload thread would, then waiting, as a frame's display does.
fn upload_run(
    gpu: &Gpu,
    uploader: &mut Uploader,
    input: &[Frame],
    slots: &[GpuFrame],
    frames: usize,
) -> UploadRun {
    let mut run = UploadRun::default();
    let (mut gpu_sum, mut gpu_n) = (0.0, 0usize);
    let mut last = allocator(gpu).map(|a| a.0);
    for _ in 0..frames {
        for (f, s) in input.iter().zip(slots) {
            run.cpu_ms += f64::from(uploader.upload(gpu, f, s).unwrap().cpu_ms);
        }
        wait(gpu);
        if let Some(ms) = uploader.poll(gpu) {
            gpu_sum += f64::from(ms);
            gpu_n += 1;
        }
        if let Some((blocks, reserved)) = allocator(gpu) {
            if last.is_some_and(|l| l != blocks) {
                run.block_changes += 1;
            }
            last = Some(blocks);
            let (lo, hi) = run.blocks.unwrap_or((blocks, blocks));
            run.blocks = Some((lo.min(blocks), hi.max(blocks)));
            run.reserved_max = run.reserved_max.max(reserved);
        }
    }
    run.cpu_ms /= frames as f64;
    run.gpu_ms = (gpu_n > 0).then(|| gpu_sum / gpu_n as f64);
    run
}

/// The GPU time of `record`, `passes` times, each in its own submission
/// and waited for: the mean, or `None` without timestamps.
fn gpu_pass_ms(
    gpu: &Gpu,
    passes: usize,
    mut record: impl FnMut(&mut wgpu::CommandEncoder, &mut GpuTimers),
) -> Option<f64> {
    let mut timers = GpuTimers::new(gpu, "bench")?;
    let mut sum = 0.0;
    let mut n = 0usize;
    for _ in 0..passes {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        record(&mut enc, &mut timers);
        gpu.queue.submit([enc.finish()]);
        wait(gpu);
        if let Some(ms) = timers.poll() {
            sum += f64::from(ms);
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

/// Records `values` (one per run) as `name`, or says why there are none.
fn record_or_not(name: &str, values: &[Option<f64>], unit: &str, why: &str) {
    let v: Vec<f64> = values.iter().flatten().copied().collect();
    if v.len() == values.len() && !v.is_empty() {
        bench::record_runs(name, &v, unit);
    } else {
        bench::not_measured(name, why);
    }
}

/// `bench_upload_5k7`'s body (M1 plan section 4.4) on `gpu`. For each
/// input (8- and 10-bit 5.7K frames, and an 8-bit and a 10-bit pair of
/// 8K's 3840² frames) and each upload path, `plan.frames` steps per run:
/// the CPU ms per step, the GPU ms of the ingest and mips, and the
/// allocator's blocks, whose count changing from step to step is the
/// plan's second trigger for the belt. Then, on the 5.7K slots: the mips
/// alone (so the ingest is the upload's span less them) and the
/// projection at each output size, rectilinear 90° and a 300° tiny
/// planet. Every number is a run's mean, recorded as the median and
/// range of `plan.runs` runs after one to warm up (D20).
fn upload_bench(gpu: &Gpu, plan: &BenchPlan) {
    let started = std::time::Instant::now();
    let tag = |s: (u32, u32)| format!("{}x{}", s.0, s.1);
    let inputs = [
        (
            format!("{} 8-bit NV12", tag(plan.single)),
            vec![noise(Kind::Nv12, plan.single, 0, 1)],
        ),
        (
            format!("{} 10-bit P010", tag(plan.single)),
            vec![noise(Kind::P010, plan.single, 0, 2)],
        ),
        (
            format!("{} 8-bit NV12 pair", tag(plan.pair)),
            vec![
                noise(Kind::Nv12, plan.pair, 0, 3),
                noise(Kind::Nv12, plan.pair, 0, 4),
            ],
        ),
        (
            format!("{} 10-bit P010 pair", tag(plan.pair)),
            vec![
                noise(Kind::P010, plan.pair, 0, 5),
                noise(Kind::P010, plan.pair, 0, 6),
            ],
        ),
    ];
    let no_timer = "no timestamps between passes";
    let slot_of =
        |f: &Frame| GpuFrame::new(gpu, &SlotShape::of(&f.layout, f.size).unwrap()).unwrap();
    // Each input's and path's median CPU time per step, for the trigger.
    let mut medians: Vec<(&str, UploadPath, f64)> = Vec::new();
    for (what, input) in &inputs {
        for path in [UploadPath::WriteTexture, UploadPath::StagingBelt] {
            let mut uploader = Uploader::with_path(gpu, path);
            let slots: Vec<GpuFrame> = input.iter().map(slot_of).collect();
            let runs: Vec<UploadRun> = (0..=plan.runs)
                .map(|_| upload_run(gpu, &mut uploader, input, &slots, plan.frames))
                .skip(1)
                .collect();
            let name = format!("upload {what}, {path:?}");
            let cpu: Vec<f64> = runs.iter().map(|r| r.cpu_ms).collect();
            bench::record_runs(&format!("{name}: CPU per step"), &cpu, "ms");
            let gpu_ms: Vec<Option<f64>> = runs.iter().map(|r| r.gpu_ms).collect();
            let per = if input.len() > 1 {
                "GPU ingest and mips per frame"
            } else {
                "GPU ingest and mips"
            };
            record_or_not(&format!("{name}: {per}"), &gpu_ms, "ms", no_timer);
            match runs.iter().map(|r| r.blocks).collect::<Option<Vec<_>>>() {
                Some(b) => {
                    let changes: Vec<f64> = runs.iter().map(|r| r.block_changes as f64).collect();
                    bench::record_runs(
                        &format!("{name}: steps that changed the allocator's blocks"),
                        &changes,
                        "steps",
                    );
                    let lo = b.iter().map(|x| x.0).min().unwrap_or(0);
                    let hi = b.iter().map(|x| x.1).max().unwrap_or(0);
                    let reserved = runs.iter().map(|r| r.reserved_max).max().unwrap_or(0);
                    bench::note(format!(
                        "{name}: {lo} to {hi} allocator blocks over {} runs of {} steps, at most {:.0} MiB reserved",
                        plan.runs,
                        plan.frames,
                        reserved as f64 / f64::from(1u32 << 20)
                    ));
                }
                None => {
                    bench::not_measured(&format!("{name}: allocator blocks"), "no allocator report")
                }
            }
            medians.push((what.as_str(), path, median(&cpu)));
            assert_eq!(runs.len(), plan.runs);
        }
    }
    // The plan's first trigger, for the 5.7K frames: write_texture above
    // 1.5 times the belt's CPU time, or above 4 ms a frame.
    for (what, _) in inputs.iter().take(2) {
        let of = |path| {
            medians
                .iter()
                .find(|m| m.0 == what && m.1 == path)
                .map_or(f64::NAN, |m| m.2)
        };
        let (w, b) = (of(UploadPath::WriteTexture), of(UploadPath::StagingBelt));
        bench::note(format!(
            "upload {what}: write_texture {w:.2} ms against the belt's {b:.2} ms a frame; the CPU trigger \
             (over 1.5 times, or over 4 ms) {}",
            if w > 1.5 * b || w > 4.0 {
                "holds"
            } else {
                "doesn't hold"
            }
        ));
    }
    // The GPU's own passes on the 5.7K slots.
    let projector = Projector::new(gpu);
    let views = [
        (
            "rectilinear 90°",
            ViewParams::new(
                glam::DQuat::IDENTITY,
                Lens::new(Projection::Rectilinear, 90f64.to_radians()),
            ),
        ),
        (
            "tiny planet 300°",
            ViewParams::new(
                glam::DQuat::from_rotation_x(-std::f64::consts::FRAC_PI_2),
                Lens::new(Projection::Stereographic, 300f64.to_radians()),
            ),
        ),
    ];
    for (what, input) in inputs.iter().take(2) {
        let mut uploader = Uploader::new(gpu);
        let slot = slot_of(&input[0]);
        uploader.upload(gpu, &input[0], &slot).unwrap();
        wait(gpu);
        let mips: Vec<Option<f64>> = (0..=plan.runs)
            .map(|_| {
                gpu_pass_ms(gpu, plan.passes, |enc, t| {
                    let span = t.begin(enc);
                    uploader.record_mips(enc, &slot);
                    if let Some(span) = span {
                        t.end(enc, span);
                    }
                })
            })
            .skip(1)
            .collect();
        record_or_not(&format!("mips {what}: GPU"), &mips, "ms", no_timer);
        for &size in &plan.outputs {
            let target = LinearTarget::new(gpu, size).unwrap();
            for (view_name, view) in &views {
                let ms: Vec<Option<f64>> = (0..=plan.runs)
                    .map(|_| {
                        gpu_pass_ms(gpu, plan.passes, |enc, t| {
                            projector
                                .record(
                                    enc,
                                    &slot,
                                    view,
                                    &target,
                                    Filter::Isotropic,
                                    DebugMode::Off,
                                    Some(t),
                                )
                                .unwrap();
                        })
                    })
                    .skip(1)
                    .collect();
                record_or_not(
                    &format!("project {what}, {view_name} to {}: GPU", tag(size)),
                    &ms,
                    "ms",
                    no_timer,
                );
            }
        }
    }
    bench::note(format!(
        "bench_upload_5k7 ran on {} ({:?}, {} {}) in {:.0} s",
        gpu.caps.info.name,
        gpu.caps.info.device_type,
        gpu.caps.info.driver,
        gpu.caps.info.driver_info,
        started.elapsed().as_secs_f64()
    ));
}

#[test]
fn upload_bench_smoke() {
    // Tier 1: the bench's every path runs on lavapipe, small.
    upload_bench(
        gpu(),
        &BenchPlan {
            single: (640, 320),
            pair: (320, 320),
            outputs: [(64, 36), (128, 72)],
            frames: 3,
            passes: 2,
            runs: 1,
        },
    );
}

#[test]
#[ignore = "tier2: benchmark, needs a GPU"]
fn bench_upload_5k7() {
    // About 0.3 GB of slots, scratch and belt chunks on the GPU and 0.3
    // GB of frames in memory: well inside an 8 GB RX 7600.
    let gpu = common::hardware_gpu();
    upload_bench(
        &gpu,
        &BenchPlan {
            single: (5760, 2880),
            pair: (3840, 3840),
            outputs: [(1920, 1080), (3840, 2160)],
            frames: 300,
            passes: 60,
            runs: bench::runs(),
        },
    );
}

#[test]
fn formats_without_mips_still_upload() {
    // A 1×1 frame: one level, and chroma of one texel.
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    for kind in [Kind::Nv12, Kind::I444p12, Kind::Gray] {
        let frame = noise(kind, (1, 1), 0, 9);
        let slot = slot_for(&frame, 8);
        assert!(slot.shape().planes.iter().all(|p| p.levels == 1));
        uploader.upload(gpu, &frame, &slot).unwrap();
        let got = levels(&slot);
        assert_eq!(got[0][0].2.len(), 1);
        assert_eq!(
            slot.shape().planes[0].format,
            if kind == Kind::I444p12 {
                PlaneFormat::R16F
            } else {
                PlaneFormat::R8
            }
        );
    }
}
