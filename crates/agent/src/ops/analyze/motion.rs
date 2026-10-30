//! `analyze motion`: the video decoded as look decodes it (D39's auto),
//! each frame's luma area-averaged to [`MOTION_GRID`], and how much it
//! changed from the frame before: the mean absolute difference of the
//! grids, 0 to 255 (code values at 8 bits). Cuts are frames that change
//! far more than the frames around them.

use serde_json::{Value, json};
use spherecut_core::video::{SampleBytes, VideoPlanes};
use spherecut_core::{Time, TimeSpec};
use spherecut_media::Source;
use spherecut_media::backend::SeekMode;
use spherecut_media::probe::{ProbeReport, StreamInfo};

use super::{Measured, Pace, Span, listed, median, none_in_time, round, secs};
use crate::contract::codes::warnings as placement;
use crate::error::{ToolError, codes};
use crate::limits::{CUT_FLOOR, CUT_OVER_MEDIAN, CUT_WINDOW, MOTION_GRID};
use crate::ops::{Ctx, look};
use crate::still::{self, Failure, Still};

pub(super) fn run(
    src: Source<'_>,
    report: &ProbeReport,
    stream: &StreamInfo,
    span: &Span,
    cx: &Ctx,
) -> Result<Measured, ToolError> {
    let stop = still::Stop::new(cx.call.token(), cx.call.interrupt());
    let opts = still::Options {
        // Any video: motion doesn't draw it.
        equirect: true,
        stream: Some(stream.index),
        ..still::Options::default()
    };
    let probed = still::Probed {
        report: report.clone(),
        stream: stream.clone(),
    };
    let mut warnings = Vec::new();
    let mut pace = Pace::new(cx);
    let stopped = |cx: &Ctx| match cx.call.check() {
        Err(e) if e.code == codes::TIMED_OUT => none_in_time(cx),
        Err(e) => e,
        Ok(()) => ToolError::new(codes::CANCELLED, "the call was stopped"),
    };
    // The GPU look renders on picks the node VA-API decodes on, as for
    // look and doctor.
    let gpu = look::gpu().ok();
    let mut s = Still::open(src, &probed, gpu.as_deref(), &opts, &stop, &mut warnings).map_err(
        |f: Failure| {
            if f.code == codes::CANCELLED {
                stopped(cx)
            } else {
                look::file_failed(&f)
            }
        },
    )?;
    let info = s.info().clone();
    let first = s
        .seek(TimeSpec::Time(span.from), SeekMode::Exact, &mut warnings)
        .map_err(|f| {
            if f.code == codes::CANCELLED {
                stopped(cx)
            } else {
                look::time_failed(&f, "from", &info)
            }
        })?;
    let decode = s.decode_info(&first);
    let total = span.end() - span.start();
    let mut frame = Some(first.frame);
    let mut prev: Option<Vec<f32>> = None;
    let mut times = Vec::new();
    let mut diffs: Vec<(Time, f64)> = Vec::new();
    let mut halted = false;
    let mut broke = None;
    while let Some(f) = frame.take() {
        let t = f.time;
        if t >= span.to {
            break;
        }
        let system;
        let planes: &dyn VideoPlanes = match f.to_system() {
            Some(Ok(down)) => {
                system = down;
                &system
            }
            Some(Err(_)) => {
                broke = Some(t);
                break;
            }
            None => &*f,
        };
        let Some(grid) = luma(planes) else {
            return Err(ToolError::new(
                codes::DECODE_FAILED,
                "has frames in a form motion can't read",
            )
            .at("file")
            .next("probe it with detail \"full\""));
        };
        if let Some(p) = &prev {
            diffs.push((t, mad(p, &grid)));
        }
        times.push(t);
        prev = Some(grid);
        cx.call
            .progress(t.to_secs_f64() - span.start(), Some(total), Some("motion"));
        if pace.stop()? {
            halted = true;
            break;
        }
        frame = match s.next(&mut warnings) {
            Ok(f) => f,
            Err(f) if f.code == codes::CANCELLED => {
                pace.stopped_for_time()?;
                halted = true;
                None
            }
            Err(_) => {
                broke = Some(t);
                None
            }
        };
    }
    let done = match (halted, times.last()) {
        (false, _) => span.n,
        (true, Some(&t)) => span.done_by(t),
        (true, None) => 0,
    };
    if done == 0 {
        return Err(none_in_time(cx));
    }
    let (buckets, mut summary) = measure(&times, &diffs, span, done);
    summary["decode"] = serde_json::to_value(&decode)
        .map_err(|e| ToolError::internal(format!("decode info: {e}")))?;
    // Where the picture sits on the sphere doesn't matter here.
    warnings.retain(|w| {
        ![
            placement::STEREO_LEFT_EYE,
            placement::STEREO_UNKNOWN,
            placement::CROP_APPLIED,
            placement::POSE_NOT_APPLIED,
        ]
        .contains(&w.code)
    });
    let mut warnings = look::warnings_json(&warnings);
    if let Some(t) = broke {
        warnings.push(json!({
            "code": "damaged",
            "message": format!(
                "the video doesn't decode past {} s: what comes after isn't measured",
                secs(t.to_secs_f64())
            ),
        }));
    }
    Ok(Measured {
        buckets,
        summary,
        stopped: (done < span.n).then_some(done),
        warnings,
    })
}

/// Plane 0's samples (luma; green for RGB) area-averaged to
/// [`MOTION_GRID`] (or the picture's size, if smaller), 0 to 255. `None`
/// for a picture whose plane can't be read.
pub(super) fn luma(frame: &dyn VideoPlanes) -> Option<Vec<f32>> {
    let layout = frame.layout();
    let desc = *layout.planes.first()?;
    let plane = frame.plane(0)?;
    let (w, h) = (plane.width as usize, plane.height as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let (gw, gh) = (MOTION_GRID.0.min(w), MOTION_GRID.1.min(h));
    let col: Vec<usize> = (0..w).map(|x| x * gw / w).collect();
    let mut per_col = vec![0u64; gw];
    for &c in &col {
        per_col[c] += 1;
    }
    let mut per_row = vec![0u64; gh];
    let mut sum = vec![0u64; gw * gh];
    let step = usize::from(desc.components) * desc.bytes.get();
    let row_bytes = desc.row_bytes(plane.width);
    for y in 0..h {
        let cy = y * gh / h;
        per_row[cy] += 1;
        let row = plane.row(u32::try_from(y).ok()?, row_bytes)?;
        let cells = &mut sum[cy * gw..(cy + 1) * gw];
        match desc.bytes {
            SampleBytes::One => {
                for (x, &c) in col.iter().enumerate() {
                    cells[c] += u64::from(row[x * step]);
                }
            }
            SampleBytes::Two => {
                for (x, &c) in col.iter().enumerate() {
                    let i = x * step;
                    cells[c] += u64::from(u16::from_le_bytes([row[i], row[i + 1]]));
                }
            }
        }
    }
    let depth = u32::from(layout.bit_depth).clamp(8, 16);
    let full = match desc.bytes {
        SampleBytes::One => 255.0,
        SampleBytes::Two if layout.msb_aligned => (((1u32 << depth) - 1) << (16 - depth)) as f64,
        SampleBytes::Two => ((1u32 << depth) - 1) as f64,
    };
    Some(
        sum.iter()
            .enumerate()
            .map(|(i, &s)| {
                let n = per_col[i % gw] * per_row[i / gw];
                (s as f64 / n.max(1) as f64 * 255.0 / full) as f32
            })
            .collect(),
    )
}

/// The mean absolute difference of two grids.
pub(super) fn mad(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from((x - y).abs()))
        .sum::<f64>()
        / a.len() as f64
}

/// The times of the frames whose change is over [`CUT_OVER_MEDIAN`]
/// times the median of the [`CUT_WINDOW`] around it, and over
/// [`CUT_FLOOR`].
fn cuts<T: Copy>(diffs: &[(T, f64)]) -> Vec<T> {
    let half = CUT_WINDOW / 2;
    (0..diffs.len())
        .filter_map(|i| {
            let d = diffs[i].1;
            if d <= CUT_FLOOR {
                return None;
            }
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(diffs.len());
            let mut window: Vec<f64> = diffs[lo..hi].iter().map(|(_, d)| *d).collect();
            let m = median(&mut window).unwrap_or(0.0);
            (d > CUT_OVER_MEDIAN * m).then_some(diffs[i].0)
        })
        .collect()
}

/// The first `done` buckets, and the summary over them.
fn measure(times: &[Time], diffs: &[(Time, f64)], span: &Span, done: usize) -> (Vec<Value>, Value) {
    let in_done = |t: Time| span.bucket(t).filter(|&b| b < done);
    let mut frames = vec![0usize; done];
    for &t in times {
        if let Some(b) = in_done(t) {
            frames[b] += 1;
        }
    }
    let diffs: Vec<(Time, f64)> = diffs
        .iter()
        .copied()
        .filter(|(t, _)| in_done(*t).is_some())
        .collect();
    let cut_times = cuts(&diffs);
    let mut per: Vec<Vec<f64>> = vec![Vec::new(); done];
    for &(t, d) in &diffs {
        if let Some(b) = in_done(t) {
            per[b].push(d);
        }
    }
    let stats = |v: &[f64]| -> (Value, Value) {
        if v.is_empty() {
            return (Value::Null, Value::Null);
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (json!(round(mean, 2)), json!(round(max, 2)))
    };
    let buckets = (0..done)
        .map(|i| {
            let (mean, max) = stats(&per[i]);
            let cuts = cut_times.iter().filter(|&&t| in_done(t) == Some(i)).count();
            json!({
                "from": secs(span.bucket_start(i)),
                "frames": frames[i],
                "mean": mean,
                "max": max,
                "cuts": cuts,
            })
        })
        .collect();
    let all: Vec<f64> = diffs.iter().map(|(_, d)| *d).collect();
    let (mean, max) = stats(&all);
    let max_at = diffs
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(t, _)| *t);
    let summary = json!({
        "frames": frames.iter().sum::<usize>(),
        "mean": mean,
        "max": max,
        "max_at": max_at,
        "cuts": listed(&cut_times),
    });
    (buckets, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spherecut_core::Rational;
    use spherecut_core::video::{
        Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, PixelLayout, PlaneData,
        PlaneDesc, Primaries, Range, Transfer,
    };

    /// One plane of `w` × `h` samples from `at`, 8 or 16 bits.
    #[derive(Debug)]
    struct Gray {
        layout: PixelLayout,
        bytes: Vec<u8>,
        size: (u32, u32),
        color: ColorInfo,
    }

    impl Gray {
        fn new(w: u32, h: u32, depth: u8, msb: bool, at: impl Fn(u32, u32) -> u16) -> Gray {
            let two = depth > 8;
            let mut bytes = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let v = at(x, y);
                    if two {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    } else {
                        bytes.push(v as u8);
                    }
                }
            }
            Gray {
                layout: PixelLayout {
                    name: "gray".into(),
                    planes: vec![PlaneDesc {
                        components: 1,
                        bytes: if two {
                            SampleBytes::Two
                        } else {
                            SampleBytes::One
                        },
                        log2_sub: (0, 0),
                    }],
                    family: ColorFamily::Gray,
                    bit_depth: depth,
                    msb_aligned: msb,
                },
                bytes,
                size: (w, h),
                color: ColorInfo {
                    codes: ColorCodes::UNSPECIFIED,
                    range: Range::Full,
                    matrix: Matrix::Identity,
                    transfer: Transfer::Sdr,
                    primaries: Primaries::Bt709,
                    siting: ChromaSiting::Left,
                    assumed: Assumed::default(),
                },
            }
        }
    }

    impl VideoPlanes for Gray {
        fn layout(&self) -> &PixelLayout {
            &self.layout
        }
        fn size(&self) -> (u32, u32) {
            self.size
        }
        fn plane(&self, index: usize) -> Option<PlaneData<'_>> {
            (index == 0).then(|| PlaneData {
                bytes: &self.bytes,
                stride: self.bytes.len() / self.size.1 as usize,
                width: self.size.0,
                height: self.size.1,
            })
        }
        fn color(&self) -> &ColorInfo {
            &self.color
        }
    }

    #[test]
    fn luma_is_area_averaged_to_the_grid() {
        // 128 × 64: each cell is 2 × 2 samples; the left half 0, the
        // right 200.
        let g = Gray::new(128, 64, 8, false, |x, _| if x < 64 { 0 } else { 200 });
        let grid = luma(&g).unwrap();
        assert_eq!(grid.len(), 64 * 32);
        assert_eq!(grid[0], 0.0);
        assert_eq!(grid[63], 200.0);
        // An odd size: cells of unequal sizes still average.
        let g = Gray::new(100, 50, 8, false, |_, _| 77);
        assert!(luma(&g).unwrap().iter().all(|&v| (v - 77.0).abs() < 1e-4));
    }

    #[test]
    fn deeper_samples_scale_to_eight_bits() {
        let lsb = Gray::new(64, 32, 10, false, |_, _| 1023);
        assert!((luma(&lsb).unwrap()[0] - 255.0).abs() < 1e-3);
        let msb = Gray::new(64, 32, 10, true, |_, _| 512 << 6);
        assert!((luma(&msb).unwrap()[0] - 512.0 * 255.0 / 1023.0).abs() < 1e-3);
    }

    #[test]
    fn a_frame_unlike_its_neighbours_is_a_cut() {
        // Steady motion of about 1, a cut at frame 20 and back at 21.
        let diffs: Vec<(f64, f64)> = (1..60)
            .map(|i| {
                let d = if i == 20 || i == 21 {
                    90.0
                } else {
                    1.0 + (i % 3) as f64 * 0.2
                };
                (i as f64 / 30.0, d)
            })
            .collect();
        let c = cuts(&diffs);
        assert_eq!(c, vec![20.0 / 30.0, 21.0 / 30.0]);
        // A pan of 20 a frame throughout is fast, not a cut.
        let pan: Vec<(f64, f64)> = (1..60).map(|i| (i as f64, 20.0)).collect();
        assert!(cuts(&pan).is_empty());
        // Over 6× the median but under the floor is not a cut either.
        let mut low: Vec<(f64, f64)> = (1..60).map(|i| (i as f64, 1.0)).collect();
        low[30].1 = 11.0;
        assert!(cuts(&low).is_empty());
    }

    #[test]
    fn buckets_split_frames_and_differences() {
        let span = Span {
            from: Time::ZERO,
            to: Time::from_secs(2),
            n: 2,
        };
        let times: Vec<Time> = (0..60)
            .map(|i| Time::seconds(Rational::new(i, 30).unwrap()))
            .collect();
        let diffs: Vec<(Time, f64)> = times[1..]
            .iter()
            .map(|&t| (t, if t < Time::from_secs(1) { 2.0 } else { 4.0 }))
            .collect();
        let (b, s) = measure(&times, &diffs, &span, 2);
        assert_eq!(b[0]["frames"], 30);
        assert_eq!(b[0]["mean"], 2.0);
        assert_eq!(b[1]["mean"], 4.0);
        assert_eq!(s["frames"], 60);
        assert_eq!(s["max"], 4.0);
        assert_eq!(s["cuts"]["count"], 0);
        let (b, s) = measure(&times, &diffs, &span, 1);
        assert_eq!(b.len(), 1);
        assert_eq!(s["frames"], 30);
        assert_eq!(s["max"], 2.0);
    }
}
