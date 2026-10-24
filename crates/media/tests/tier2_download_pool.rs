//! Tier 2: frames come off the device into pooled buffers (M1 plan
//! sections 3.5 and 12.2, DECISIONS D30).
//!
//! `ffi::download_into_pool` copies each frame a hardware decoder hands
//! out into a buffer from an `AVBufferPool`, which gets it back when the
//! frame is dropped. Once the pool holds what playback keeps alive, a
//! download allocates nothing: after 30 frames to warm up, the decoder's
//! pool must allocate no buffer more (`DecodeStats::download_buffers`),
//! and it must have allocated some, so that the frames did come through
//! it.
//!
//! The decoding thread's minor page faults per frame go in the report as
//! a measurement, not a limit (D30). They can't check the pool: at
//! 3840x1920 glibc hands a freed block of up to 32 MiB back from its heap,
//! so even a fresh buffer per frame need not fault more (none per frame,
//! measured with glibc 2.39). And they are mostly the driver's: on odin's
//! RX 7600, VA-API took 116 per NV12 frame and 227 per P010 frame with no
//! buffer allocated after the warm-up, about one per 95 KB copied, as it
//! maps each surface to read it.
//!
//! VA-API runs on the backend's own device on the render node under test.
//! Vulkan Video is the optional path (amendment 9): its test skips with
//! FFmpeg's reason for the clips the driver can't decode, and runs on the
//! backend's Vulkan device behind that render node, else the first GPU,
//! never llvmpipe (D38).
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use spherecut_core::{FrameRate, StopToken};
use spherecut_media::backend::{
    DecodeMode, DecodePath, Fallback, FfmpegBackend, HwKind, MediaBackend, NextFrame, VideoDecode,
    VideoRequest,
};
use spherecut_media::decode::open_vulkan_device;
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::{HwError, MediaError, capture_hw_decode_log};
use spherecut_testkit::{bench, tier2};

/// Frames decoded before counting, while the pool fills.
const WARM_UP: usize = 30;
/// Frames counted.
const MEASURED: usize = 60;

/// A test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tier2-pool-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The clips' size: one 2:1 4K frame.
const SIZE: (usize, usize) = (3840, 1920);

/// The megabytes a frame of [`SIZE`] at `bits` bits comes off the device
/// in, NV12 or P010.
fn frame_mb(bits: u32) -> f64 {
    let sample = if bits > 8 { 2.0 } else { 1.0 };
    (SIZE.0 * SIZE.1) as f64 * 1.5 * sample / 1e6
}

/// 3840x1920 HEVC at `bits` bits (NV12 or P010 off the device), long
/// enough to warm up and count: a ramp that moves 8 pixels a frame, every
/// frame cut from one buffer so that making it stays cheap in debug
/// builds.
fn clip(dir: &TempDir, bits: u32) -> PathBuf {
    let (w, h) = SIZE;
    let frames = WARM_UP + MEASURED + 10;
    let settings = VideoSettings {
        encoder: "libx265".into(),
        width: w as u32,
        height: h as u32,
        rate: FrameRate::FPS_30,
        layout: if bits > 8 {
            PixelLayout::Yuv420p10
        } else {
            PixelLayout::Yuv420p
        },
        options: [("preset", "ultrafast"), ("x265-params", "log-level=error")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let path = dir.0.join(format!("hevc{bits}-3840.mp4"));
    let mut out = match Mp4Writer::create(&path, &settings, None) {
        Ok(out) => out,
        Err(e @ spherecut_media::encode::EncodeError::NoEncoder(_)) => {
            tier2::skip(format!("can't generate the test clip: {e}"))
        }
        Err(e) => panic!("{e}"),
    };
    let ramp: Vec<u16> = (0..w * h + 8 * frames).map(|p| (p % 220) as u16).collect();
    for i in 0..frames {
        let luma = &ramp[8 * i..8 * i + w * h];
        let planes = if bits > 8 {
            let chroma = vec![512; w * h / 4];
            YuvPlanes::U16([
                luma.iter().map(|v| 64 + 4 * v).collect(),
                chroma.clone(),
                chroma,
            ])
        } else {
            let chroma = vec![128; w * h / 4];
            YuvPlanes::U8([
                luma.iter().map(|&v| 16 + v as u8).collect(),
                chroma.clone(),
                chroma,
            ])
        };
        out.push_video(&planes).unwrap();
    }
    out.finish().unwrap()
}

/// This thread's minor page faults so far: field 10 of
/// `/proc/thread-self/stat`. A hardware decoder decodes and downloads on
/// the thread that asks for the frame, so other threads, such as other
/// tests', don't count.
fn minor_faults() -> u64 {
    let stat =
        std::fs::read_to_string("/proc/thread-self/stat").expect("reading /proc/thread-self/stat");
    // The command name, field 2, is in parentheses and may hold spaces;
    // after it come state, ppid, pgrp, session, tty_nr, tpgid, flags and
    // minflt.
    let rest = &stat[stat.rfind(')').expect("no command name in the stat line") + 1..];
    rest.split_whitespace()
        .nth(7)
        .and_then(|f| f.parse().ok())
        .unwrap_or_else(|| panic!("no minflt in {stat:?}"))
}

/// Fail unless [`minor_faults`] counts: writing to fresh memory faults it
/// in, a page or a huge page at a time. 64 MiB is over the most glibc
/// takes from its heap (32 MiB on 64-bit), so the block is always a new
/// mapping: 8 MiB came back already faulted in from a block an earlier
/// test had freed, and counted nothing.
fn check_fault_counter() {
    let before = minor_faults();
    let mut fresh = vec![0u8; 64 << 20];
    for i in (0..fresh.len()).step_by(4096) {
        fresh[i] = 1;
    }
    std::hint::black_box(&fresh);
    let counted = minor_faults() - before;
    assert!(
        counted > 0,
        "/proc/thread-self/stat counted no minor faults for 64 MiB written"
    );
}

/// A backend that decodes on its own device of kind `kind`, and never
/// falls back.
struct Hw {
    backend: FfmpegBackend,
    kind: HwKind,
    /// The device, and why that one, for messages.
    about: String,
}

impl Hw {
    fn open(&self, path: &Path) -> Result<Box<dyn VideoDecode>, MediaError> {
        let req = VideoRequest {
            decode: DecodeMode::Hardware {
                kind: self.kind,
                device: None,
                fallback: Fallback::Fail,
            },
            ..VideoRequest::default()
        };
        self.backend.open_video(path, &req)
    }
}

/// The backend's VA-API device, on the render node under test.
fn vaapi() -> Hw {
    let node = tier2::require_render_node();
    let backend = FfmpegBackend::on_render_node(node);
    if let Err(e) = backend.device(HwKind::VaApi) {
        panic!("{e} (run `spherecut doctor` for the fix)");
    }
    let about = format!("VA-API on {}", backend.render_node());
    Hw {
        backend,
        kind: HwKind::VaApi,
        about,
    }
}

/// The backend's Vulkan device, on the GPU behind the render node under
/// test, else the first GPU; the test skips without one.
/// `open_vulkan_device` says which GPU that is, or that there is none;
/// the backend must then open it too.
fn vulkan() -> Hw {
    tier2::require_hw_run();
    let node = tier2::render_node();
    let about = match open_vulkan_device(node.as_deref()) {
        Ok(c) if c.used.is_cpu() => tier2::skip(format!(
            "no GPU for Vulkan Video: FFmpeg's Vulkan device is {c}"
        )),
        Ok(c) => c.to_string(),
        Err(
            e @ MediaError::Hardware {
                reason: HwError::Vulkan(_),
                ..
            },
        ) => panic!("{e}"),
        Err(e) => tier2::skip(format!("no Vulkan device for FFmpeg: {e}")),
    };
    let backend = match node {
        Some(node) => FfmpegBackend::on_render_node(node),
        None => FfmpegBackend::new(),
    };
    if let Err(e) = backend.device(HwKind::Vulkan) {
        panic!(
            "FfmpegBackend opened no Vulkan device, where open_vulkan_device found {about}: {e}"
        );
    }
    eprintln!("Vulkan device: {about}");
    Hw {
        backend,
        kind: HwKind::Vulkan,
        about: format!("Vulkan on {about}"),
    }
}

/// Why `hw`'s device turns `path` down, if it does, with what FFmpeg
/// logged meanwhile, which is the only place that says why ("Device does
/// not support decoding hevc!"). Any other error fails the test.
fn turned_down(hw: &Hw, path: &Path) -> Option<String> {
    let (r, log) = capture_hw_decode_log(|| {
        hw.open(path)
            .and_then(|mut d| d.next_frame(&StopToken::never()))
            .map(drop)
    });
    let ffmpeg = if log.is_empty() {
        String::new()
    } else {
        format!(" (FFmpeg: {})", log.join(" "))
    };
    match r {
        Ok(()) => None,
        Err(MediaError::HwUnsupported { refusal, .. }) => Some(format!("{refusal}{ffmpeg}")),
        Err(e) => panic!("{} on {}: {e}{ffmpeg}", path.display(), hw.about),
    }
}

/// What [`MEASURED`] frames after [`WARM_UP`] cost, each dropped once the
/// next is out as playback drops them.
struct Counted {
    /// Minor page faults per frame on the decoding thread.
    faults_per_frame: f64,
    /// Buffers the download pool allocated by the end of the warm-up, and
    /// by the end.
    allocated: (u64, u64),
}

fn count(hw: &Hw, path: &Path) -> Result<Counted, MediaError> {
    let mut d = hw.open(path)?;
    let on = DecodePath::Hardware { kind: hw.kind };
    let step = |d: &mut Box<dyn VideoDecode>| -> Result<(), MediaError> {
        match d.next_frame(&StopToken::never())? {
            NextFrame::Frame(f) => {
                assert_eq!(f.decoded_on, on);
                Ok(())
            }
            other => panic!(
                "{}: {other:?} before frame {}",
                path.display(),
                WARM_UP + MEASURED
            ),
        }
    };
    for _ in 0..WARM_UP {
        step(&mut d)?;
    }
    let warm = d.stats().download_buffers;
    let before = minor_faults();
    for _ in 0..MEASURED {
        step(&mut d)?;
    }
    let faults = minor_faults() - before;
    Ok(Counted {
        faults_per_frame: faults as f64 / MEASURED as f64,
        allocated: (warm, d.stats().download_buffers),
    })
}

/// Count buffers and faults on 8-bit and 10-bit clips on `hw`, note the
/// numbers for the report, and fail if the pool wasn't used or allocated
/// after the warm-up. Vulkan Video
/// skips, after the clips it decodes, with the reason for those it can't.
fn check_pool(hw: &Hw) {
    check_fault_counter();
    let dir = TempDir::new(hw.kind.ffmpeg_name());
    let mut problems = Vec::new();
    let mut refused = Vec::new();
    for (bits, format) in [(8, "NV12"), (10, "P010")] {
        let path = clip(&dir, bits);
        if let Some(why) = turned_down(hw, &path) {
            assert_eq!(hw.kind, HwKind::Vulkan, "{}: {why}", hw.about);
            refused.push(format!("HEVC {bits}-bit 3840x1920: {why}"));
            continue;
        }
        let c =
            count(hw, &path).unwrap_or_else(|e| panic!("{} on {}: {e}", path.display(), hw.about));
        let (warm, end) = c.allocated;
        let counted = format!(
            "{:.1} minor faults per 3840x1920 {format} frame ({:.1} per MB), over {MEASURED} \
             frames after {WARM_UP}; the download pool allocated {warm} buffers in those \
             {WARM_UP} and {} after",
            c.faults_per_frame,
            c.faults_per_frame / frame_mb(bits),
            end - warm
        );
        bench::note(format!("{}: {counted}", hw.about));
        if warm == 0 {
            problems.push(format!("{format}: {counted}: the frames bypassed the pool"));
        }
        if end > warm {
            problems.push(format!(
                "{format}: {counted}: the pool doesn't reuse its buffers"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "on {}:\n{}",
        hw.about,
        problems.join("\n")
    );
    if !refused.is_empty() {
        tier2::skip(format!("{} can't decode {}", hw.about, refused.join("; ")));
    }
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main and Main10"]
fn vaapi_download_pool_reuses_buffers() {
    check_pool(&vaapi());
}

#[test]
#[ignore = "tier2: needs Vulkan Video HEVC decode (optional path)"]
fn vulkan_download_pool_reuses_buffers() {
    check_pool(&vulkan());
}
