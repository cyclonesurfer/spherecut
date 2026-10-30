//! Tier 1, on lavapipe: planes into slots (M1 plan sections 4.2 and 4.4).
//! Level 0 holds each plane's codes, deep ones through the ingest pass
//! with P010's shift; each mip level is the area-weighted mean of the one
//! above at odd sizes, an 8-bit one rounded to the nearest code, ties to
//! even, the reference's code for code up to 16384 texels a side; both
//! upload paths fill a slot alike; bad frames and wrong slots are
//! refused; a frame already on the GPU fills a slot as an upload does,
//! and planes already there (slice 5b's imports, made here by wgpu) fill
//! one byte for byte as their bytes do; and the upload bench runs, small (its Tier 2 form, `bench_upload_5k7`,
//! is here too).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Frame, Kind, Rng, gpu, lavapipe_unorm8, radv_unorm8, unrounded_mip_mean};
use spherecut_core::video::{ChromaSiting, Matrix, Range, SampleBytes};
use spherecut_render::readback::{f16_to_f32, read_texture, round_f16};
use spherecut_render::{
    AdapterChoice, DebugMode, Filter, Gpu, GpuFrame, GpuTimers, LinearTarget, Placement,
    PlaneFormat, Projector, SlotShape, UploadPath, Uploader, ViewParams, dmabuf, reference, upload,
    vkshare,
};
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_synth::{Pattern, render_equirect};
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
                // An 8-bit level's mean is of its codes, which an f32
                // code / 255 holds only to 1e-5 of a code.
                let codes: Vec<f32> = above.iter().map(|v| (v * 255.0).round()).collect();
                for j in 0..*dh {
                    for x in 0..*dw {
                        for c in 0..comps {
                            let want = area_mean(above, (*sw, *sh), comps, (*dw, *dh), (x, j), c);
                            let g = f64::from(
                                this[(j as usize * *dw as usize + x as usize) * comps + c],
                            );
                            // At 8 bits the nearest code, ties to even,
                            // exactly (D31). For deep planes the nearest
                            // half of the mean, unless the mean is so near
                            // a tie that the shader's f32 sum could fall
                            // on either side of it.
                            if p.format.is_deep() {
                                assert!(
                                    g == round_f16(want) || near_a_tie(want),
                                    "{kind:?} plane {i} level {l} ({x}, {j}) c{c}: {g} for {want}, \
                                     whose nearest half is {}",
                                    round_f16(want)
                                );
                            } else {
                                let m = area_mean(&codes, (*sw, *sh), comps, (*dw, *dh), (x, j), c);
                                let code = nearest_code(m, true);
                                assert!(
                                    (g * 255.0).round() == f64::from(code),
                                    "{kind:?} plane {i} level {l} ({x}, {j}) c{c}: {} for {m}, \
                                     whose nearest code is {code}",
                                    g * 255.0
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
fn eight_bit_mips_are_exact_at_the_largest_sizes() {
    // Where Σ ox·oy·code passes a u32, which the pass splits as it sums
    // (`shaders/mips.wgsl`): R8 and Rg8 (NV24) 16383 × 2051, odd both
    // ways, so that a destination texel covers parts of three source
    // texels each way; R8 16384 × 2049, the widest a texture comes; and R8
    // 2051 × 16383, as tall. Every code is 128 or more, so that each
    // destination texel's sum is at least 128·Sx·Sy, past 2^32. Level 1
    // is the reference's, code for code. The control sums in a u32,
    // wrapping, and is off at every texel.
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    for (kind, size) in [
        (Kind::Nv24, (16383, 2051)),
        (Kind::Gray, (16384, 2049)),
        (Kind::Gray, (2051, 16383)),
    ] {
        let mut frame = noise(kind, size, 0, 13);
        for (bytes, _, _) in &mut frame.planes {
            for b in bytes.iter_mut() {
                *b |= 0x80;
            }
        }
        let slot = slot_for(&frame, 2);
        uploader.upload(gpu, &frame, &slot).unwrap();
        for (i, p) in slot.shape().planes.iter().enumerate() {
            let at = format!("{kind:?} {}x{} plane {i} ({:?})", size.0, size.1, p.format);
            let comps = p.format.components() as usize;
            let level0 = codes(&frame, i);
            let area = u64::from(p.size.0) * u64::from(p.size.1);
            assert!(128 * area > u64::from(u32::MAX), "{at}");
            let got = read_texture(gpu, slot.plane_texture(i).unwrap(), 1).unwrap();
            let (w, h, want) = reference::halve_codes(&level0, p.size, comps);
            assert_eq!((got.width, got.height), (w, h), "{at}");
            let off = got.data.iter().zip(&want).filter(|(a, b)| a != b).count();
            assert_eq!(off, 0, "{at}: {off} texels of level 1 off the reference's");
            let span = |j: u32, ss: u32| {
                let ds = (ss / 2).max(1);
                let (lo, hi) = (j * ss, j * ss + ss);
                (lo / ds..=(hi - 1) / ds).map(move |x| (x, hi.min((x + 1) * ds) - lo.max(x * ds)))
            };
            let (_, _, wrapped) = halve_by(p.size, comps, |(x, y), c| {
                let mut n = 0u32;
                for (sy, oy) in span(y, p.size.1) {
                    for (sx, ox) in span(x, p.size.0) {
                        let code =
                            level0[(sy as usize * p.size.0 as usize + sx as usize) * comps + c];
                        n = n.wrapping_add((ox * oy).wrapping_mul(u32::from(code)));
                    }
                }
                ((u64::from(n) * 2 + area) / (2 * area)) as u8
            });
            let right = wrapped.iter().zip(&want).filter(|(a, b)| a == b).count();
            assert_eq!(right, 0, "{at}: a u32 sum gets {right} texels right");
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
    // Every 10-bit code, high-aligned (P010) and low (yuv420p10), every
    // 12-bit code and every 16-bit code, through the ingest pass: each
    // texel must be code / (2^b − 1) rounded to the nearest half, ties to
    // even. The shader rounds it itself; lavapipe's own conversion into
    // an f16 target truncates, which leaves about half the codes one half
    // below (DECISIONS D31). Only 16 bits reach the subnormal halves
    // (codes 1 to 3, below 2^−14), and among them are codes an f32
    // division would put on the wrong side of a tie (65455 and 65519).
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    for (kind, size) in [
        (Kind::P010, (64, 16)),
        (Kind::I420p10, (64, 16)),
        (Kind::I444p12, (64, 64)),
        (Kind::Gray16, (256, 256)),
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

/// `m` codes to the nearest code, a tie to even or up. An area mean of
/// codes is n / (Sx·Sy), at least 1 / (2·Sx·Sy) from a tie it isn't on,
/// which is 1e-6 at a million texels, so an f64 mean within 1e-9 of a tie
/// is taken as on it.
fn nearest_code(m: f64, ties_even: bool) -> u8 {
    let k = m.floor();
    let past = m - k - 0.5;
    let up = if past.abs() < 1e-9 {
        !ties_even || k % 2.0 == 1.0
    } else {
        past > 0.0
    };
    (k + f64::from(u8::from(up))) as u8
}

/// Plane `i` of an 8-bit `frame`, its rows without their padding.
fn codes(frame: &Frame, i: usize) -> Vec<u8> {
    let (bytes, stride, (w, h)) = &frame.planes[i];
    let row = *w as usize * usize::from(frame.layout.planes[i].components);
    (0..*h as usize)
        .flat_map(|y| bytes[y * stride..y * stride + row].iter().copied())
        .collect()
}

/// A level: its size and its codes, components interleaved.
type Codes = (u32, u32, Vec<u8>);

/// Every level of 8-bit plane `i` of `slot`, as codes.
fn level_codes(slot: &GpuFrame, i: usize) -> Vec<Codes> {
    let t = slot.plane_texture(i).unwrap();
    (0..slot.shape().planes[i].levels)
        .map(|l| {
            let texels = read_texture(gpu(), t, l).unwrap();
            (texels.width, texels.height, texels.data)
        })
        .collect()
}

/// A chain of `levels` levels from `level0`, each made from the one above
/// by `halve`.
fn chain_of(level0: Codes, levels: u32, halve: impl Fn(&[u8], (u32, u32)) -> Codes) -> Vec<Codes> {
    let mut chain = vec![level0];
    while chain.len() < levels as usize {
        let (w, h, above) = chain.last().unwrap();
        let next = halve(above, (*w, *h));
        chain.push(next);
    }
    chain
}

/// Each texel of a level made from `above` by `code`, given its
/// destination texel and component.
fn halve_by(above: (u32, u32), comps: usize, code: impl Fn((u32, u32), usize) -> u8) -> Codes {
    let (dw, dh) = ((above.0 / 2).max(1), (above.1 / 2).max(1));
    let mut out = Vec::with_capacity(dw as usize * dh as usize * comps);
    for j in 0..dh {
        for i in 0..dw {
            for c in 0..comps {
                out.push(code((i, j), c));
            }
        }
    }
    (dw, dh, out)
}

/// Texels of `got` that differ from `want`'s, level by level from level 1.
fn differ(got: &[Codes], want: &[Codes]) -> Vec<usize> {
    got.iter()
        .zip(want)
        .skip(1)
        .map(|(g, w)| {
            assert_eq!((g.0, g.1), (w.0, w.1));
            g.2.iter().zip(&w.2).filter(|(a, b)| a != b).count()
        })
        .collect()
}

#[test]
fn eight_bit_mips_are_the_references_code_for_code() {
    // The mip pass rounds each 8-bit mean to its code itself, ties to
    // even, as the reference does (`reference::halve_codes`, D31), so that
    // a driver that converts to nearest stores that code. On lavapipe every
    // level of R8 and Rg8 planes, at even and odd sizes, is the
    // reference's, code for code. Ties are common: a 2×2 mean of codes is
    // a whole code or a quarter, a half or three quarters past one. The
    // controls each differ from the reference: the pass as it was, its
    // f32 mean left to RADV's export (packed to f16 toward zero, then to
    // the nearest code), which odin's RX 7600 stored; and the exact mean
    // with its ties taken up, as the reference's f64 `round` took them.
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    let grid = |w, h| {
        let img = render_equirect(Pattern::Grid, w, h, 0, 2);
        common::frame(
            &img,
            Kind::Nv12,
            common::color(Range::Limited, Matrix::Bt709, ChromaSiting::Left),
            8,
        )
    };
    let frames = [
        ("noise", noise(Kind::Nv12, (256, 128), 4, 3)),
        ("noise", noise(Kind::Nv12, (45, 23), 12, 7)),
        ("noise", noise(Kind::Nv12, (70, 38), 0, 9)),
        ("grid", grid(1024, 512)),
        ("grid", grid(1002, 502)),
    ];
    for (what, frame) in &frames {
        let slot = slot_for(frame, 8);
        uploader.upload(gpu, frame, &slot).unwrap();
        for (i, p) in slot.shape().planes.iter().enumerate() {
            let at = format!(
                "{what} {}x{} plane {i} ({:?})",
                frame.size.0, frame.size.1, p.format
            );
            let comps = p.format.components() as usize;
            let level0 = (p.size.0, p.size.1, codes(frame, i));
            let got = level_codes(&slot, i);
            assert!(got.len() >= 5, "{at}: {} levels", got.len());
            let want = chain_of(level0.clone(), p.levels, |c, s| {
                reference::halve_codes(c, s, comps)
            });
            assert_eq!(got[0], want[0], "{at}: level 0");
            for (l, (g, w)) in got.iter().zip(&want).enumerate().skip(1) {
                let off: Vec<(usize, u8, u8)> =
                    g.2.iter()
                        .zip(&w.2)
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .map(|(n, (a, b))| (n, *a, *b))
                        .take(4)
                        .collect();
                assert!(
                    (g.0, g.1) == (w.0, w.1) && off.is_empty(),
                    "{at} level {l} ({}x{}): texels off the reference's, such as {off:?}",
                    g.0,
                    g.1
                );
            }
            // How many of each level's means lie on a tie.
            let ties: Vec<usize> = want
                .windows(2)
                .map(|w| {
                    let (sw, sh, above) = &w[0];
                    let f: Vec<f32> = above.iter().map(|&c| f32::from(c)).collect();
                    let (dw, dh, _) = &w[1];
                    let mut n = 0;
                    for j in 0..*dh {
                        for x in 0..*dw {
                            for c in 0..comps {
                                let m = area_mean(&f, (*sw, *sh), comps, (*dw, *dh), (x, j), c);
                                n += usize::from((m - m.floor() - 0.5).abs() < 1e-9);
                            }
                        }
                    }
                    n
                })
                .collect();
            // The controls.
            let radv = chain_of(level0.clone(), p.levels, |c, s| {
                halve_by(s, comps, |d, k| {
                    radv_unorm8(unrounded_mip_mean(c, s, comps, d, k))
                })
            });
            let lavapipe = chain_of(level0.clone(), p.levels, |c, s| {
                halve_by(s, comps, |d, k| {
                    lavapipe_unorm8(unrounded_mip_mean(c, s, comps, d, k))
                })
            });
            let up = chain_of(level0, p.levels, |c, s| {
                let f: Vec<f32> = c.iter().map(|&v| f32::from(v)).collect();
                let d = ((s.0 / 2).max(1), (s.1 / 2).max(1));
                halve_by(s, comps, |at, k| {
                    nearest_code(area_mean(&f, s, comps, d, at, k), false)
                })
            });
            let (radv, lavapipe, up) = (
                differ(&radv, &want),
                differ(&lavapipe, &want),
                differ(&up, &want),
            );
            eprintln!(
                "{at}: ties by level {ties:?}; off the reference by level: the old pass through \
                 RADV's export {radv:?}, through lavapipe's {lavapipe:?}; ties up {up:?}"
            );
            // A mean over an odd area (Sx·Sy, as the pass counts it) never
            // ties, so 45 × 23 has none at level 1.
            assert!(ties.iter().sum::<usize>() > 0, "{at}: no ties");
            assert!(
                radv[0] > 0,
                "{at}: RADV's export of the old pass is the reference's"
            );
            assert!(
                up.iter().sum::<usize>() > 0,
                "{at}: ties up are the reference's"
            );
        }
    }
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

/// `frame`'s planes as textures, as `dmabuf::Importer` makes them: one a
/// plane, whole, in the import's format and usage (and `COPY_DST`, to
/// fill them here). What lavapipe can't import, it can make.
fn on_gpu(frame: &Frame) -> Vec<wgpu::Texture> {
    let gpu = gpu();
    let whole = SlotShape::of(&frame.layout, frame.size).unwrap();
    whole
        .planes
        .iter()
        .zip(&frame.planes)
        .map(|(p, (bytes, stride, _))| {
            let size = wgpu::Extent3d {
                width: p.size.0,
                height: p.size.1,
                depth_or_array_layers: 1,
            };
            let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("plane on the GPU"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: dmabuf::imported_format(p.format),
                usage: dmabuf::USAGE | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                t.as_image_copy(),
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(*stride as u32),
                    rows_per_image: None,
                },
                size,
            );
            t
        })
        .collect()
}

#[test]
fn planes_on_the_gpu_fill_a_slot_as_their_bytes_do() {
    // Slice 5b's import path with textures in place of imports: the same
    // levels and the same projection, byte for byte, for the layouts
    // VA-API exports (NV12, P010) and for three deep planes LSB-aligned;
    // whole frames and a stereo frame's shown eye.
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    let top_bottom = Placement {
        stereo: spherecut_render::Stereo::TopBottom,
        ..Placement::FULL
    };
    for kind in [Kind::Nv12, Kind::P010, Kind::I420p10] {
        for (size, pad) in [((96u32, 48u32), 0usize), ((70, 34), 34)] {
            let mut frame = noise(kind, size, pad, 31);
            frame.color = common::color(Range::Limited, Matrix::Bt709, ChromaSiting::Left);
            let planes = on_gpu(&frame);
            for placement in [Placement::FULL, top_bottom] {
                let shape = SlotShape::of(&frame.layout, placement.shown(frame.size))
                    .unwrap()
                    .with_levels(4);
                let uploaded = GpuFrame::new(gpu, &shape).unwrap();
                uploader
                    .upload_placed(gpu, &frame, &placement, &uploaded)
                    .unwrap();
                let imported = GpuFrame::new(gpu, &shape).unwrap();
                let stats = uploader
                    .upload_imported(gpu, &frame, &planes, &placement, &imported)
                    .unwrap();
                assert_eq!(stats.bytes, 0);
                let what = format!("{kind:?} {size:?} {:?}", placement.stereo);
                assert_eq!(levels(&imported), levels(&uploaded), "{what}");
                assert_eq!(project(&imported), project(&uploaded), "{what}");
            }
        }
    }
}

#[test]
fn planes_on_the_gpu_that_dont_fit_are_refused() {
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    let frame = noise(Kind::P010, (64, 32), 0, 8);
    let planes = on_gpu(&frame);
    let slot = slot_for(&frame, 1);
    let refused = |uploader: &mut Uploader, planes: &[wgpu::Texture], slot: &GpuFrame| {
        uploader
            .upload_imported(gpu, &frame, planes, &Placement::FULL, slot)
            .unwrap_err()
            .code()
    };
    assert_eq!(refused(&mut uploader, &planes[..1], &slot), "invalid_input");
    // NV12's planes for a P010 frame: 8-bit where integers are wanted.
    let nv12 = on_gpu(&noise(Kind::Nv12, (64, 32), 0, 8));
    assert_eq!(refused(&mut uploader, &nv12, &slot), "invalid_input");
    // A plane of the wrong size, and one that can't be copied from.
    let small = on_gpu(&noise(Kind::P010, (64, 30), 0, 8));
    let mixed = [planes[0].clone(), small[1].clone()];
    assert_eq!(refused(&mut uploader, &mixed, &slot), "invalid_input");
    let no_copy = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: planes[1].size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: planes[1].format(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let mixed = [planes[0].clone(), no_copy];
    assert_eq!(refused(&mut uploader, &mixed, &slot), "invalid_input");
    // A slot for another size.
    let other = slot_for(&noise(Kind::P010, (64, 34), 0, 8), 1);
    assert_eq!(refused(&mut uploader, &planes, &other), "internal");
    // And the good planes still fill the slot.
    uploader
        .upload_imported(gpu, &frame, &planes, &Placement::FULL, &slot)
        .unwrap();
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
        // The other tests poll the same device, and the one whose poll
        // finds the read-back done runs its callback after letting the
        // device go, so the reading can come a moment after this wait.
        let timed = (0..1000)
            .find_map(|_| {
                uploader.poll(gpu).or_else(|| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    None
                })
            })
            .expect("a GPU time once idle");
        let timed = f64::from(timed);
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

/// The allocator's memory blocks, by size, and its allocated and
/// reserved bytes. gpu-allocator names no block, so a block freed and
/// another of its size made between two reports look alike; the bench
/// reports often enough that a step's own blocks show.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blocks {
    sizes: Vec<u64>,
    allocated: u64,
    reserved: u64,
}

/// The allocator's report, when it gives one.
fn allocator(gpu: &Gpu) -> Option<Blocks> {
    gpu.device.generate_allocator_report().map(|r| {
        let mut sizes: Vec<u64> = r.blocks.iter().map(|b| b.size).collect();
        sizes.sort_unstable();
        Blocks {
            sizes,
            allocated: r.total_allocated_bytes,
            reserved: r.total_reserved_bytes,
        }
    })
}

/// How an upload run paces its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pace {
    /// Each step waits for the GPU, as a frame's display would: the GPU
    /// time of each step's ingest and mips is read back.
    Waited,
    /// Steps go into `n` slots in turn and only poll, as the upload
    /// thread does, so `n` of them may be in flight (the slot ring keeps
    /// two ready ahead of the shown one); a step waits only for the one
    /// `n` back, whose slot it fills.
    InFlight(usize),
}

/// What one run of upload steps cost, per step.
#[derive(Debug, Default)]
struct UploadRun {
    cpu_ms: f64,
    /// The mean of the spans read back: each step's ingest and mips, or
    /// the second frame's of a pair (the two are the same size). Waited
    /// runs only.
    gpu_ms: Option<f64>,
    /// Steps in which the allocator's blocks changed: the report taken
    /// as the step's uploads return (their staging made, the GPU not yet
    /// done) or once it waits differs from the one before it, so a block
    /// made for a step and freed when it is done counts, as plan section
    /// 4.4's "blocks allocated and freed per frame" trigger means.
    churn: usize,
    /// The fewest and most blocks in any report, and the most bytes
    /// reserved and allocated in one.
    blocks: Option<(usize, usize)>,
    reserved_max: u64,
    allocated_max: u64,
    /// The bytes allocated once settled, before the run's first step.
    allocated_settled: u64,
}

impl UploadRun {
    /// Takes the allocator's report: whether its blocks differ from
    /// `last`'s, which it then replaces.
    fn sample(&mut self, gpu: &Gpu, last: &mut Option<Blocks>) -> bool {
        let Some(now) = allocator(gpu) else {
            return false;
        };
        let n = now.sizes.len();
        let (lo, hi) = self.blocks.unwrap_or((n, n));
        self.blocks = Some((lo.min(n), hi.max(n)));
        self.reserved_max = self.reserved_max.max(now.reserved);
        self.allocated_max = self.allocated_max.max(now.allocated);
        let changed = last.as_ref().is_some_and(|l| l.sizes != now.sizes);
        *last = Some(now);
        changed
    }
}

/// `frames` steps, each uploading every frame of `input` into its slot as
/// the upload thread would, paced by `pace`. `slots` has a slot for each
/// frame of `input`, for each step that may be in flight.
fn upload_run(
    gpu: &Gpu,
    uploader: &mut Uploader,
    input: &[Frame],
    slots: &[Vec<GpuFrame>],
    frames: usize,
    pace: Pace,
) -> UploadRun {
    let mut run = UploadRun::default();
    let (mut gpu_sum, mut gpu_n) = (0.0, 0usize);
    wait(gpu);
    let mut last = None;
    run.sample(gpu, &mut last);
    run.allocated_settled = last.as_ref().map_or(0, |b| b.allocated);
    let depth = match pace {
        Pace::Waited => 1,
        Pace::InFlight(n) => n,
    };
    assert_eq!(slots.len(), depth);
    // Each step's submission, so a later step can wait for it.
    let mut done = std::collections::VecDeque::new();
    for step in 0..frames {
        let mut churned = false;
        if done.len() == depth
            && let Some(index) = done.pop_front()
        {
            let _ = gpu.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            });
            churned |= run.sample(gpu, &mut last);
        }
        for (f, s) in input.iter().zip(&slots[step % depth]) {
            run.cpu_ms += f64::from(uploader.upload(gpu, f, s).unwrap().cpu_ms);
        }
        // In flight: the step's write_texture staging is allocated, and
        // nothing of it is freed until the GPU is done with it.
        churned |= run.sample(gpu, &mut last);
        // An empty submission is done when everything before it is.
        done.push_back(gpu.queue.submit([]));
        match pace {
            Pace::Waited => {
                wait(gpu);
                if let Some(ms) = uploader.poll(gpu) {
                    gpu_sum += f64::from(ms);
                    gpu_n += 1;
                }
                churned |= run.sample(gpu, &mut last);
                done.clear();
            }
            Pace::InFlight(_) => {
                let _ = uploader.poll(gpu);
            }
        }
        run.churn += usize::from(churned);
    }
    wait(gpu);
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

/// `frame` with its rows as the pooled download lays a decoded frame out
/// (`DOWNLOAD_ROW_ALIGN` in media's `ffi.rs`): each plane's row rounded up
/// to 256 bytes. Which copy each path takes depends on the stride:
/// wgpu-core's `write_texture` copies in one go only when the stride is
/// the row rounded up to the device's `optimalBufferCopyRowPitchAlignment`
/// (128 on lavapipe), the belt only when it is the row rounded up to 256,
/// so at a 5760-byte row the two copy row by row and in one go here,
/// where tight rows would have them the other way round.
fn pooled(mut frame: Frame) -> Frame {
    for (desc, (bytes, stride, (w, h))) in frame.layout.planes.iter().zip(frame.planes.iter_mut()) {
        let row = desc.row_bytes(*w);
        let pitch = row.next_multiple_of(256);
        let mut b = vec![0u8; pitch * (*h as usize - 1) + row];
        for y in 0..*h as usize {
            b[y * pitch..y * pitch + row].copy_from_slice(&bytes[y * *stride..y * *stride + row]);
        }
        *bytes = b;
        *stride = pitch;
    }
    frame
}

/// How many steps `Pace::InFlight` keeps in flight: the slot ring's two
/// ready ahead of the shown one (plan section 4.2).
const IN_FLIGHT: usize = 2;

/// `bench_upload_5k7`'s body (M1 plan section 4.4) on `gpu`. For each
/// input (8- and 10-bit 5.7K frames, and an 8-bit and a 10-bit pair of
/// 8K's 3840² frames, with the pooled download's rows) and each upload
/// path, `plan.frames` steps per run, twice: waiting for each step, for
/// the CPU ms per step and the GPU ms of the ingest and mips; and with
/// [`IN_FLIGHT`] steps in flight, only polling, as the upload thread
/// does. Each counts the steps that made or freed allocator blocks, the
/// plan's second trigger for the belt, from reports taken both while a
/// step is in flight and once it is done. Then, on the 5.7K slots: the
/// mips alone (so the ingest is the upload's span less them) and the
/// projection at each output size, rectilinear 90° and a 300° tiny
/// planet. Every number is a run's mean, recorded as the median and
/// range of `plan.runs` runs after one to warm up (D20).
fn upload_bench(gpu: &Gpu, plan: &BenchPlan) {
    let started = std::time::Instant::now();
    let tag = |s: (u32, u32)| format!("{}x{}", s.0, s.1);
    let frame = |kind, size, seed| pooled(noise(kind, size, 0, seed));
    let inputs = [
        (
            format!("{} 8-bit NV12", tag(plan.single)),
            vec![frame(Kind::Nv12, plan.single, 1)],
        ),
        (
            format!("{} 10-bit P010", tag(plan.single)),
            vec![frame(Kind::P010, plan.single, 2)],
        ),
        (
            format!("{} 8-bit NV12 pair", tag(plan.pair)),
            vec![
                frame(Kind::Nv12, plan.pair, 3),
                frame(Kind::Nv12, plan.pair, 4),
            ],
        ),
        (
            format!("{} 10-bit P010 pair", tag(plan.pair)),
            vec![
                frame(Kind::P010, plan.pair, 5),
                frame(Kind::P010, plan.pair, 6),
            ],
        ),
    ];
    // As a decoded frame comes from the pooled download.
    for (what, input) in &inputs {
        for f in input {
            for (desc, (_, stride, (w, _))) in f.layout.planes.iter().zip(&f.planes) {
                let row = desc.row_bytes(*w);
                assert_eq!(*stride, row.next_multiple_of(256), "{what}");
            }
        }
    }
    let no_timer = "no timestamps between passes";
    let slot_of =
        |f: &Frame| GpuFrame::new(gpu, &SlotShape::of(&f.layout, f.size).unwrap()).unwrap();
    // Each input's and path's median CPU time per step, for the first
    // trigger, and whether any run of it made or freed blocks, for the
    // second.
    let mut medians: Vec<(&str, UploadPath, f64)> = Vec::new();
    let mut churned: Vec<(&str, UploadPath, bool)> = Vec::new();
    for (what, input) in &inputs {
        for path in [UploadPath::WriteTexture, UploadPath::StagingBelt] {
            let mut uploader = Uploader::with_path(gpu, path);
            for pace in [Pace::Waited, Pace::InFlight(IN_FLIGHT)] {
                let depth = if pace == Pace::Waited { 1 } else { IN_FLIGHT };
                let slots: Vec<Vec<GpuFrame>> = (0..depth)
                    .map(|_| input.iter().map(slot_of).collect())
                    .collect();
                let runs: Vec<UploadRun> = (0..=plan.runs)
                    .map(|_| upload_run(gpu, &mut uploader, input, &slots, plan.frames, pace))
                    .skip(1)
                    .collect();
                let name = match pace {
                    Pace::Waited => format!("upload {what}, {path:?}"),
                    Pace::InFlight(n) => format!("upload {what}, {path:?}, {n} in flight"),
                };
                let cpu: Vec<f64> = runs.iter().map(|r| r.cpu_ms).collect();
                bench::record_runs(&format!("{name}: CPU per step"), &cpu, "ms");
                if pace == Pace::Waited {
                    let gpu_ms: Vec<Option<f64>> = runs.iter().map(|r| r.gpu_ms).collect();
                    let per = if input.len() > 1 {
                        "GPU ingest and mips per frame"
                    } else {
                        "GPU ingest and mips"
                    };
                    record_or_not(&format!("{name}: {per}"), &gpu_ms, "ms", no_timer);
                    medians.push((what.as_str(), path, median(&cpu)));
                }
                match runs.iter().map(|r| r.blocks).collect::<Option<Vec<_>>>() {
                    Some(b) => {
                        let churn: Vec<f64> = runs.iter().map(|r| r.churn as f64).collect();
                        bench::record_runs(
                            &format!("{name}: steps that made or freed allocator blocks"),
                            &churn,
                            "steps",
                        );
                        churned.push((what.as_str(), path, runs.iter().any(|r| r.churn > 0)));
                        let mib = |b: u64| b as f64 / f64::from(1u32 << 20);
                        let lo = b.iter().map(|x| x.0).min().unwrap_or(0);
                        let hi = b.iter().map(|x| x.1).max().unwrap_or(0);
                        let max = |f: fn(&UploadRun) -> u64| runs.iter().map(f).max().unwrap_or(0);
                        bench::note(format!(
                            "{name}: {lo} to {hi} allocator blocks over {} runs of {} steps, at most \
                             {:.0} MiB reserved and {:.0} MiB allocated ({:.0} MiB settled)",
                            plan.runs,
                            plan.frames,
                            mib(max(|r| r.reserved_max)),
                            mib(max(|r| r.allocated_max)),
                            mib(max(|r| r.allocated_settled)),
                        ));
                    }
                    None => bench::not_measured(
                        &format!("{name}: allocator blocks"),
                        "no allocator report",
                    ),
                }
                assert_eq!(runs.len(), plan.runs);
            }
        }
    }
    // The plan's second trigger: write_texture making or freeing blocks
    // within a step, waited or in flight.
    for (what, _) in &inputs {
        let blocks = churned
            .iter()
            .find(|c| c.0 == what && c.1 == UploadPath::WriteTexture)
            .map(|c| c.2);
        if let Some(b) = blocks {
            bench::note(format!(
                "upload {what}: write_texture {} allocator blocks within a step; the block trigger {}",
                if b {
                    "makes or frees"
                } else {
                    "neither makes nor frees"
                },
                if b { "holds" } else { "doesn't hold" }
            ));
        }
    }
    // The first trigger, for the 5.7K frames, as H1 restated it (D31):
    // the belt's CPU time under 0.9 times write_texture's. The plan's
    // "above 1.5 times, or above 4 ms" held on odin through its 4 ms arm
    // where the belt cost the same.
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
             (the belt under 0.9 times) {}",
            if b < 0.9 * w { "holds" } else { "doesn't hold" }
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
fn the_bench_sees_a_steps_own_blocks() {
    // The allocator's report once a step is done can't show what the
    // step made and freed: wgpu-core frees a write_texture's staging when
    // the GPU is done with it, and gpu-allocator then frees a block left
    // empty unless it is the last of its kind. So the bench reports while
    // the step is in flight too. On lavapipe (wgpu-core 30, gpu-allocator
    // 0.28) a pair of 3840² NV12 frames, 22 MB each, settles in one 64
    // MiB block, and every step makes a 128 MiB block for its staging and
    // frees it once done: reported only once settled, no step changed
    // anything. A device of its own, so that no other test's memory is
    // in its allocator's reports.
    let gpu = &Gpu::headless(&AdapterChoice::Cpu).unwrap();
    let input: Vec<Frame> = (0..2)
        .map(|i| pooled(noise(Kind::Nv12, (3840, 3840), 0, 20 + i)))
        .collect();
    let frame_bytes = input[0]
        .planes
        .iter()
        .map(|p| p.0.len() as u64)
        .sum::<u64>();
    let slot_of =
        |f: &Frame| GpuFrame::new(gpu, &SlotShape::of(&f.layout, f.size).unwrap()).unwrap();
    let steps = 4;
    for pace in [Pace::Waited, Pace::InFlight(IN_FLIGHT)] {
        let depth = if pace == Pace::Waited { 1 } else { IN_FLIGHT };
        let slots: Vec<Vec<GpuFrame>> = (0..depth)
            .map(|_| input.iter().map(slot_of).collect())
            .collect();
        let mut uploader = Uploader::with_path(gpu, UploadPath::WriteTexture);
        let run = upload_run(gpu, &mut uploader, &input, &slots, steps, pace);
        eprintln!("{pace:?}: {run:?}");
        // In flight, a step's staging is there to see: a frame's worth,
        // less what small buffers came and went meanwhile.
        assert!(
            run.allocated_max >= run.allocated_settled + frame_bytes * 9 / 10,
            "{pace:?}: {run:?}"
        );
        // Waited, each step makes a block and frees it. (With two in
        // flight the staging block may never empty, which the bench
        // reports as it finds it.)
        if pace == Pace::Waited {
            assert_eq!(run.churn, steps, "{run:?}");
            assert_eq!(run.blocks, Some((1, 2)), "{run:?}");
        }
    }
}

#[test]
#[ignore = "tier2: benchmark, needs a GPU"]
fn bench_upload_5k7() {
    // At most about 1 GB on the GPU (two sets of slots for the 10-bit
    // pair, 236 MB, its scratch, two steps' staging and the belt's 64
    // MiB chunks) and 0.3 GB of frames in memory: well inside an 8 GB RX
    // 7600.
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

/// `frame`'s shown planes in a GPU buffer, laid out as a
/// `vkshare::Copier` lays its copies out: what path B hands the uploader.
fn in_buffer(frame: &Frame, placement: &Placement) -> (wgpu::Buffer, Vec<vkshare::BufferPlane>) {
    let gpu = gpu();
    let regions = upload::shown_regions(frame, placement).unwrap();
    let (planes, size) = vkshare::buffer_layout(&regions);
    let mut bytes = vec![0u8; size as usize];
    for ((at, r), (data, stride, _)) in planes.iter().zip(&regions).zip(&frame.planes) {
        let row = (r.size.0 * r.bytes_per_texel) as usize;
        for y in 0..r.size.1 as usize {
            let dst = at.offset as usize + y * at.bytes_per_row as usize;
            bytes[dst..dst + row].copy_from_slice(&data[y * stride..y * stride + row]);
        }
    }
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("planes in a buffer"),
        size,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue.write_buffer(&buffer, 0, &bytes);
    (buffer, planes)
}

#[test]
fn planes_in_a_buffer_fill_a_slot_as_their_bytes_do() {
    // Slice 5b's path B with a buffer written here in place of the
    // copier's: the same levels and projection as the copy path, byte for
    // byte, for the layouts Vulkan Video decodes to (NV12, P010) and one
    // with three deep planes; whole frames and a stereo frame's shown eye.
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    let top_bottom = Placement {
        stereo: spherecut_render::Stereo::TopBottom,
        ..Placement::FULL
    };
    for kind in [Kind::Nv12, Kind::P010, Kind::I420p10] {
        for (size, pad) in [((96u32, 48u32), 0usize), ((70, 34), 34)] {
            let mut frame = noise(kind, size, pad, 41);
            frame.color = common::color(Range::Limited, Matrix::Bt709, ChromaSiting::Left);
            for placement in [Placement::FULL, top_bottom] {
                let (buffer, planes) = in_buffer(&frame, &placement);
                let shape = SlotShape::of(&frame.layout, placement.shown(frame.size))
                    .unwrap()
                    .with_levels(4);
                let uploaded = GpuFrame::new(gpu, &shape).unwrap();
                uploader
                    .upload_placed(gpu, &frame, &placement, &uploaded)
                    .unwrap();
                let copied = GpuFrame::new(gpu, &shape).unwrap();
                let stats = uploader
                    .upload_from_buffer(gpu, &frame, &buffer, &planes, &placement, &copied)
                    .unwrap();
                assert_eq!(stats.bytes, 0);
                let what = format!("{kind:?} {size:?} {:?}", placement.stereo);
                assert_eq!(levels(&copied), levels(&uploaded), "{what}");
                assert_eq!(project(&copied), project(&uploaded), "{what}");
            }
        }
    }
}

#[test]
fn buffers_that_dont_fit_are_refused() {
    let gpu = gpu();
    let mut uploader = Uploader::new(gpu);
    let frame = noise(Kind::P010, (64, 32), 0, 8);
    let (buffer, planes) = in_buffer(&frame, &Placement::FULL);
    let slot = slot_for(&frame, 1);
    let mut refused = |planes: &[vkshare::BufferPlane], buffer: &wgpu::Buffer, slot: &GpuFrame| {
        uploader
            .upload_from_buffer(gpu, &frame, buffer, planes, &Placement::FULL, slot)
            .unwrap_err()
            .code()
    };
    assert_eq!(refused(&planes[..1], &buffer, &slot), "invalid_input");
    // A plane of another size, rows too short or off wgpu's alignment, and
    // a plane past the buffer's end.
    let mut wrong = planes.clone();
    wrong[1].size.1 -= 1;
    assert_eq!(refused(&wrong, &buffer, &slot), "invalid_input");
    let mut short = planes.clone();
    short[0].bytes_per_row = 64;
    assert_eq!(refused(&short, &buffer, &slot), "invalid_input");
    let mut odd = planes.clone();
    odd[0].bytes_per_row = 384;
    assert_eq!(refused(&odd, &buffer, &slot), "invalid_input");
    let mut past = planes.clone();
    past[1].offset = buffer.size() - 256;
    assert_eq!(refused(&past, &buffer, &slot), "invalid_input");
    // A slot for another size.
    let other = slot_for(&noise(Kind::P010, (64, 34), 0, 8), 1);
    assert_eq!(refused(&planes, &buffer, &other), "internal");
    // And the good planes still fill the slot.
    uploader
        .upload_from_buffer(gpu, &frame, &buffer, &planes, &Placement::FULL, &slot)
        .unwrap();
}
