//! Tier 2: Pat's camera samples.
//!
//! The samples live outside the repo (`samples/<camera>/`, or
//! `SPHERECUT_SAMPLES`). Every camera file in there must probe cleanly and
//! decode on the GPU exactly as it does in software. The benchmark measures
//! whether each recording mode decodes in real time.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use spherecut_media::decode::{VideoDecoder, open_hw_device};
use spherecut_media::detect::Layout;
use spherecut_media::probe::{self, ProbeReport, StreamKind};
use spherecut_media::{HwDevice, MediaError};
use spherecut_testkit::{bench, tier2};

const EXTENSIONS: &[&str] = &["insv", "lrv", "mp4", "mov", "360", "osv"];

/// Camera files under the samples directory, sorted, skipping sidecars
/// such as `.thumbnail.bin`.
fn sample_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| {
                EXTENSIONS.contains(&x.to_string_lossy().to_ascii_lowercase().as_str())
            }) {
                out.push(p);
            }
        }
    }
    let dir = tier2::require_samples_dir();
    let mut v = Vec::new();
    walk(&dir, &mut v);
    v.sort();
    if v.is_empty() {
        tier2::skip(format!("no camera files under {}", dir.display()));
    }
    v
}

fn video_streams(r: &ProbeReport) -> Vec<usize> {
    r.streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video)
        .map(|s| s.index)
        .collect()
}

fn vaapi() -> Arc<HwDevice> {
    let node = tier2::require_render_node();
    open_hw_device("vaapi", Some(&node.to_string_lossy()))
        .unwrap_or_else(|e| panic!("{e} (run `spherecut doctor` for the fix)"))
}

#[test]
#[ignore = "tier2: needs camera samples"]
fn samples_probe_cleanly() {
    let mut problems = Vec::new();
    for p in sample_files() {
        let name = p.display();
        match probe::probe(&p) {
            Err(e) => problems.push(format!("{name}: {e}")),
            Ok(r) => {
                if !r.damage.is_empty() {
                    problems.push(format!("{name}: damaged: {:?}", r.damage));
                }
                if !r.warnings.is_empty() {
                    problems.push(format!("{name}: warnings {:?}", r.warnings));
                }
                let layout = r.camera.as_ref().map(|c| c.layout);
                if matches!(layout, None | Some(Layout::Unknown)) {
                    problems.push(format!("{name}: camera/layout not detected"));
                }
                if video_streams(&r).is_empty() {
                    problems.push(format!("{name}: no video stream"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Frames as (pts, Y/U/V planes).
type Frames = Vec<(Option<i64>, [Vec<u16>; 3])>;

fn decode_first(
    path: &Path,
    index: usize,
    hw: Option<Arc<HwDevice>>,
    n: usize,
) -> Result<Frames, MediaError> {
    let on_gpu = hw.is_some();
    let mut d = VideoDecoder::open(path, Some(index), hw)?;
    let mut out = Vec::new();
    while out.len() < n {
        let Some(f) = d.next_frame()? else { break };
        assert_eq!(f.hw_decoded, on_gpu);
        let planes = f
            .planes_u16()
            .unwrap_or_else(|| panic!("unexpected pixel format: {f:?}"));
        out.push((f.pts, planes));
    }
    Ok(out)
}

#[test]
#[ignore = "tier2: needs camera samples and a VA-API GPU"]
fn samples_vaapi_decode_matches_software() {
    let files = sample_files();
    let dev = vaapi();
    let mut problems = Vec::new();
    for p in files {
        let Ok(r) = probe::probe(&p) else { continue };
        for idx in video_streams(&r) {
            let what = format!("{} stream {idx}", p.display());
            let sw = match decode_first(&p, idx, None, 30) {
                Ok(f) => f,
                Err(e) => {
                    problems.push(format!("{what}: software decode failed: {e}"));
                    continue;
                }
            };
            let hw = match decode_first(&p, idx, Some(dev.clone()), 30) {
                Ok(f) => f,
                Err(e) => {
                    problems.push(format!("{what}: VA-API decode failed: {e}"));
                    continue;
                }
            };
            if hw.len() != sw.len() {
                problems.push(format!("{what}: {} frames vs {}", hw.len(), sw.len()));
            } else if let Some(i) = (0..sw.len()).find(|&i| sw[i] != hw[i]) {
                problems.push(format!("{what}: frame {i} differs from software"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Decode up to `frames` frames of every listed video stream of `path` at
/// once, one thread per stream as playback will, and return the slowest
/// stream's frames per second.
fn decode_rate(path: &Path, streams: &[usize], hw: Option<Arc<HwDevice>>, frames: usize) -> f64 {
    let t = Instant::now();
    let counts: Vec<usize> = std::thread::scope(|s| {
        let handles: Vec<_> = streams
            .iter()
            .map(|&idx| {
                let hw = hw.clone();
                s.spawn(move || {
                    let mut d = VideoDecoder::open(path, Some(idx), hw).unwrap();
                    let mut n = 0;
                    while n < frames && d.next_frame().unwrap().is_some() {
                        n += 1;
                    }
                    n
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    counts.iter().copied().min().unwrap_or(0) as f64 / t.elapsed().as_secs_f64()
}

#[test]
#[ignore = "tier2: benchmark, needs camera samples and a VA-API GPU"]
fn bench_samples_realtime_decode() {
    let files = sample_files();
    let dev = vaapi();
    // The longest file of each recording mode: (layout, streams, size, fps).
    let mut modes: BTreeMap<String, (PathBuf, Vec<usize>, f64, f64)> = BTreeMap::new();
    for p in files {
        let Ok(r) = probe::probe(&p) else { continue };
        let streams = video_streams(&r);
        let Some(v) = r.streams.iter().find(|s| s.kind == StreamKind::Video) else {
            continue;
        };
        let (Some(vi), Some(fps)) = (&v.video, v.avg_frame_rate) else {
            continue;
        };
        let layout = r
            .camera
            .as_ref()
            .map(|c| format!("{:?}", c.layout))
            .unwrap_or_default();
        let key = format!(
            "{layout} {}× {}x{} @ {:.2}",
            streams.len(),
            vi.width,
            vi.height,
            fps.to_f64()
        );
        let len = v.duration.map(|d| d.to_f64()).unwrap_or(0.0);
        let e = modes
            .entry(key)
            .or_insert((p.clone(), streams.clone(), fps.to_f64(), len));
        if len > e.3 {
            *e = (p, streams, fps.to_f64(), len);
        }
    }
    for (mode, (path, streams, fps, _)) in &modes {
        // Up to ten seconds of footage.
        let frames = (fps * 10.0).round() as usize;
        let hw = decode_rate(path, streams, Some(dev.clone()), frames);
        bench::record(&format!("{mode}: VA-API decode"), hw / fps, "× real time");
        let sw = decode_rate(path, streams, None, frames);
        bench::record(&format!("{mode}: software decode"), sw / fps, "× real time");
    }
}
