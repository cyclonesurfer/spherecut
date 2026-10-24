//! Tier 2: Pat's camera samples.
//!
//! The samples live outside the repo (`samples/<camera>/`, or
//! `SPHERECUT_SAMPLES`). Every camera file in there must probe cleanly and
//! decode on the GPU exactly as it does in software. The benchmark measures
//! whether each recording mode decodes in real time.
//!
//! VA-API is the hardware decoder Spherecut relies on. Vulkan Video is
//! optional: where the driver can't decode a format, its test skips with
//! FFmpeg's reason and its benchmark rows are left out with it. It runs on
//! the GPU behind VA-API's render node, or the first GPU, never on
//! llvmpipe (`open_vulkan_device`, DECISIONS D27).
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use spherecut_media::decode::{VideoDecoder, open_hw_device, open_vulkan_device};
use spherecut_media::detect::Layout;
use spherecut_media::probe::{self, ProbeReport, StreamKind};
use spherecut_media::{HwDevice, HwError, MediaError, capture_hw_decode_log};
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

/// A stream's codec and profile, e.g. "hevc (Main 10)", and its size and
/// level, e.g. "3840x3840 at level 153" (FFmpeg's numbering of levels, as
/// in its own lines).
fn stream_format(r: &ProbeReport, index: usize) -> (String, String) {
    let s = r.streams.iter().find(|s| s.index == index);
    let mut format = s
        .and_then(|s| s.codec.clone())
        .unwrap_or_else(|| "unknown codec".into());
    if let Some(p) = s.and_then(|s| s.profile.as_ref()) {
        format += &format!(" ({p})");
    }
    let mut size = s
        .and_then(|s| s.video.as_ref())
        .map_or("?".into(), |v| format!("{}x{}", v.width, v.height));
    if let Some(level) = s.and_then(|s| s.level) {
        size += &format!(" at level {level}");
    }
    (format, size)
}

fn vaapi() -> Arc<HwDevice> {
    let node = tier2::require_render_node();
    open_hw_device("vaapi", Some(&node.to_string_lossy()))
        .unwrap_or_else(|e| panic!("{e} (run `spherecut doctor` for the fix)"))
}

/// FFmpeg's Vulkan device on a GPU.
struct Vulkan {
    dev: Arc<HwDevice>,
    /// Which device it is, and why that one.
    about: String,
}

/// Why there is no Vulkan GPU to decode on.
enum NoVulkan {
    /// There is none: a test skips.
    Missing(String),
    /// Choosing it went wrong: a test fails.
    Broken(String),
}

impl NoVulkan {
    fn reason(&self) -> &str {
        match self {
            NoVulkan::Missing(r) | NoVulkan::Broken(r) => r,
        }
    }
}

/// FFmpeg's Vulkan device on the GPU behind VA-API's render node, else the
/// first GPU.
fn vulkan_gpu() -> Result<Vulkan, NoVulkan> {
    match open_vulkan_device(tier2::render_node().as_deref()) {
        Ok(c) if c.used.is_cpu() => Err(NoVulkan::Missing(format!(
            "no GPU for Vulkan Video: FFmpeg's Vulkan device is {c}"
        ))),
        Ok(c) => {
            eprintln!("Vulkan device: {c}");
            Ok(Vulkan {
                about: c.to_string(),
                dev: c.device,
            })
        }
        Err(
            e @ MediaError::Hardware {
                reason: HwError::Vulkan(_),
                ..
            },
        ) => Err(NoVulkan::Broken(e.to_string())),
        Err(e) => Err(NoVulkan::Missing(format!(
            "no Vulkan device for FFmpeg: {e}"
        ))),
    }
}

/// [`vulkan_gpu`] in a test, which skips without one.
fn vulkan() -> Vulkan {
    tier2::require_hw_run();
    match vulkan_gpu() {
        Ok(v) => v,
        Err(NoVulkan::Missing(why)) => tier2::skip(why),
        Err(NoVulkan::Broken(why)) => panic!("{why}"),
    }
}

/// Why `dev` can't decode the first frame of stream `index` of `path`, if
/// it can't: the error, and what FFmpeg logged meanwhile. Only the log
/// says why for a driver without the codec ("Device does not support
/// decoding hevc!") or the profile ("Unable to initialize video session:
/// hevc profile "Main 10" not supported!"). For a stream bigger or of a
/// higher level than the driver decodes, FFmpeg says nothing but the
/// limits it checked ("Maximum level: 153 (stream 186)"), so
/// `samples_vulkan_decode_matches_software` adds the stream's size and
/// level.
fn first_frame(path: &Path, index: usize, dev: &Arc<HwDevice>) -> Result<(), (MediaError, String)> {
    let (r, log) = capture_hw_decode_log(|| {
        VideoDecoder::open(path, Some(index), Some(dev.clone())).and_then(|mut d| d.next_frame())
    });
    let ffmpeg = if log.is_empty() {
        String::new()
    } else {
        format!(" (FFmpeg: {})", log.join(" "))
    };
    r.map(drop).map_err(|e| (e, ffmpeg))
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

#[test]
#[ignore = "tier2: needs camera samples and Vulkan Video decode (optional path)"]
fn samples_vulkan_decode_matches_software() {
    let files = sample_files();
    let vk = vulkan();
    let mut problems = Vec::new();
    // The streams the driver turned down, by format and reason: their
    // sizes and how many.
    let mut refused: BTreeMap<(String, String), (BTreeSet<String>, usize)> = BTreeMap::new();
    let mut matched = 0;
    for p in files {
        let Ok(r) = probe::probe(&p) else { continue };
        for idx in video_streams(&r) {
            let what = format!("{} stream {idx}", p.display());
            match first_frame(&p, idx, &vk.dev) {
                Ok(()) => {}
                // Vulkan Video is optional, and drivers lack profiles.
                Err((MediaError::HwUnsupported { refusal, .. }, ffmpeg)) => {
                    let (format, size) = stream_format(&r, idx);
                    let e = refused
                        .entry((format, format!("{refusal}{ffmpeg}")))
                        .or_default();
                    e.0.insert(size);
                    e.1 += 1;
                    continue;
                }
                Err((e, ffmpeg)) => {
                    problems.push(format!("{what}: Vulkan decode failed: {e}{ffmpeg}"));
                    continue;
                }
            }
            let sw = match decode_first(&p, idx, None, 30) {
                Ok(f) => f,
                Err(e) => {
                    problems.push(format!("{what}: software decode failed: {e}"));
                    continue;
                }
            };
            let hw = match decode_first(&p, idx, Some(vk.dev.clone()), 30) {
                Ok(f) => f,
                Err(e) => {
                    problems.push(format!("{what}: Vulkan decode failed: {e}"));
                    continue;
                }
            };
            if hw.len() != sw.len() {
                problems.push(format!("{what}: {} frames vs {}", hw.len(), sw.len()));
            } else if let Some(i) = (0..sw.len()).find(|&i| sw[i] != hw[i]) {
                problems.push(format!("{what}: frame {i} differs from software"));
            } else {
                matched += 1;
            }
        }
    }
    let refusals = refused
        .iter()
        .map(|((format, why), (sizes, n))| {
            let sizes: Vec<&str> = sizes.iter().map(String::as_str).collect();
            format!("{format} {} ({n} streams): {why}", sizes.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        problems.is_empty(),
        "Vulkan Video on {}:\n{}{}",
        vk.about,
        problems.join("\n"),
        if refused.is_empty() {
            String::new()
        } else {
            format!("\nVulkan Video can't decode: {refusals}")
        }
    );
    if !refused.is_empty() {
        let n: usize = refused.values().map(|(_, n)| n).sum();
        tier2::skip(format!(
            "Vulkan Video can't decode {n} of {} streams on {}; {refusals}{}",
            n + matched,
            vk.about,
            if matched > 0 {
                format!("; the other {matched} match software")
            } else {
                String::new()
            }
        ));
    }
}

/// Decode up to `frames` frames of every listed video stream of `path` at
/// once, one thread per stream as playback will, and return the slowest
/// stream's frames per second.
fn decode_rate(
    path: &Path,
    streams: &[usize],
    hw: Option<Arc<HwDevice>>,
    frames: usize,
) -> Result<f64, MediaError> {
    let t = Instant::now();
    let counts: Vec<Result<usize, MediaError>> = std::thread::scope(|s| {
        let handles: Vec<_> = streams
            .iter()
            .map(|&idx| {
                let hw = hw.clone();
                s.spawn(move || {
                    let mut d = VideoDecoder::open(path, Some(idx), hw)?;
                    let mut n = 0;
                    while n < frames && d.next_frame()?.is_some() {
                        n += 1;
                    }
                    Ok(n)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let slowest = counts
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .unwrap_or(0);
    Ok(slowest as f64 / t.elapsed().as_secs_f64())
}

#[test]
#[ignore = "tier2: benchmark, needs camera samples and a VA-API GPU"]
fn bench_samples_realtime_decode() {
    let files = sample_files();
    let dev = vaapi();
    // Vulkan Video is optional here: where it can't decode a mode, that
    // row is left out and the report says why.
    let vulkan = vulkan_gpu();
    if let Ok(vk) = &vulkan {
        bench::note(format!("Vulkan Video rows ran on {}", vk.about));
    }
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
        // Up to ten seconds of footage a run.
        let frames = (fps * 10.0).round() as usize;
        let rate =
            |hw: Option<Arc<HwDevice>>| decode_rate(path, streams, hw, frames).map(|r| r / fps);
        bench::measure(&format!("{mode}: VA-API decode"), "× real time", || {
            rate(Some(dev.clone())).unwrap_or_else(|e| panic!("{e}"))
        });
        bench::measure(&format!("{mode}: software decode"), "× real time", || {
            rate(None).unwrap_or_else(|e| panic!("{e}"))
        });
        let row = format!("{mode}: Vulkan Video decode");
        let measured = match &vulkan {
            Err(no) => Err(no.reason().to_string()),
            Ok(vk) => streams
                .iter()
                .try_for_each(|&idx| {
                    first_frame(path, idx, &vk.dev)
                        .map_err(|(e, ffmpeg)| format!("stream {idx}: {e}{ffmpeg}"))
                })
                .and_then(|()| {
                    bench::try_measure(&row, "× real time", || rate(Some(vk.dev.clone())))
                        .map_err(|e| e.to_string())
                }),
        };
        if let Err(why) = measured {
            bench::not_measured(&row, why);
        }
    }
}
