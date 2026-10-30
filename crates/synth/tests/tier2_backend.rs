//! Tier 2: the backend's hardware path on real GPUs (M1 plan sections 3.3,
//! 3.5 and 12.2, DECISIONS D29 and D30).
//!
//! Through `FfmpegBackend`, as the player opens media, a decoder on the
//! device must hand out what software does: at every seek, the frame shown
//! at the time asked for, with the pts a sequential decode gives it and
//! bit for bit software's, then the frame after it; and the same colour,
//! from the stream's tags or from D30's defaults. The device may not hand
//! the stream to software (`Fallback::Fail`): a frame it can't decode
//! fails the test.
//!
//! VA-API runs on the backend's own device on the render node under test.
//! Vulkan Video is the optional path (amendment 9): where the driver can't
//! decode a stream, it is left out, and the test skips with FFmpeg's
//! reasons once the others have passed; any other error fails it. It runs
//! on the backend's Vulkan device, the one behind that render node or the
//! first GPU, never llvmpipe (D38).
//!
//! The streams are the noisy 5.7K clips, which cost the decoder what
//! footage does, and every stream of the camera samples. The clips are
//! generated once and cached in Cargo's per-target temp dir, as
//! `tier2_hw_decode.rs`'s are. `bench_seek_latency` measures how long a
//! seek takes to hand out its frame, in software and on both devices.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use spherecut_core::video::{SampleBytes, VideoPlanes};
use spherecut_core::{FrameRate, Rounding, StopToken, Time};
use spherecut_media::backend::{
    DecodeMode, DecodePath, DecoderEvent, Exactness, Fallback, Fault, FaultPlan, FfmpegBackend,
    HwKind, HwOutput, MediaBackend, NextFrame, SeekMode, SeekOutcome, Transfer, VideoDecode,
    VideoFrame, VideoRequest,
};
use spherecut_media::decode::open_vulkan_device;
use spherecut_media::encode::{ColorTags, EncodeError};
use spherecut_media::probe::{self, StreamKind};
use spherecut_media::{HwError, MediaError, capture_hw_decode_log};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, CLIP_COLOR, Codec, Gop, Spec, SynthError};
use spherecut_testkit::{bench, tier2};

/// Random exact seeks per stream.
const SEEKS: usize = 50;

/// Random exact seeks per stream above 2880² pixels, such as the X5's 8K
/// takes: each software seek there decodes up to a GOP at about 25 fps on
/// odin, and there are ten such streams, so 50 each would add about a
/// quarter of an hour per test to hw-test for little more coverage.
const SEEKS_LARGE: usize = 20;

/// Problems one stream may report before its check stops.
const MAX_PROBLEMS: usize = 10;

/// `spec`'s clip, generated into Cargo's per-target temp dir the first
/// time. Caching by existence is safe because `media::write` renames a
/// clip into place only once it is complete (see `tier2_hw_decode.rs`).
fn cached(spec: &Spec) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tier2-media-v2");
    let path = dir.join(spec.file_name());
    if !path.exists() {
        match media::write(spec, &dir) {
            Ok(_) => {}
            // A prerequisite, not a hardware result; hw-test lists the fix.
            Err(SynthError::Encode(EncodeError::NoEncoder(e))) => {
                tier2::skip(format!("FFmpeg has no {e} encoder to generate test clips"))
            }
            Err(e) => panic!("generating {}: {e}", path.display()),
        }
    }
    path
}

/// Two seconds of the noisy 5760x2880 HEVC perf clip at `bits` bits, in
/// open GOPs of 30 frames with B-frames, as a camera records.
fn noisy(bits: u32) -> PathBuf {
    cached(&Spec {
        name: format!("t2-hevc{bits}-5k7-noisy"),
        pattern: Pattern::Grid,
        width: 5760,
        height: 2880,
        rate: FrameRate::FPS_30,
        frames: 60,
        codec: Codec::Hevc,
        bits,
        audio: Audio::None,
        spherical: true,
        gop: Some(Gop {
            keyint: 30,
            bframes: 3,
            open: true,
            radl: 0,
        }),
        noise: Some(0x5eed_0000 + u64::from(bits)),
        ..Spec::default()
    })
}

/// 30 frames of 1000x562 in open GOPs of 8 with 3 B-frames, which H.264
/// codes as 1008x576 and HEVC as 1000x568: the decoder crops every
/// picture on the right or at the bottom, on the device too.
fn cropped(codec: Codec) -> PathBuf {
    cached(&cropped_spec(codec))
}

fn cropped_spec(codec: Codec) -> Spec {
    Spec {
        name: format!("t2-{}-cropped", format!("{codec:?}").to_lowercase()),
        width: 1000,
        height: 562,
        frames: 30,
        codec,
        gop: Some(Gop {
            keyint: 8,
            bframes: 3,
            open: true,
            radl: 0,
        }),
        ..Spec::default()
    }
}

/// [`cropped`]'s clip at `bits` bits, whose stream also crops 48 columns
/// on the left, so it decodes 952 wide. FFmpeg leaves a left crop on a
/// device frame for the download to apply, and the download must cut the
/// picture software decodes, in NV12 and P010 alike.
fn left_cropped(codec: Codec, bits: u32) -> PathBuf {
    let spec = cropped_spec(codec);
    cached(&Spec {
        name: format!("{}{bits}-left", spec.name),
        bits,
        crop_left: 48,
        ..spec
    })
}

/// A small clip of `bits` bits tagged `color`, for the colour tests.
fn tagged(name: &str, codec: Codec, bits: u32, width: u32, color: ColorTags) -> PathBuf {
    cached(&Spec {
        name: name.into(),
        width,
        height: width / 2,
        frames: 12,
        codec,
        bits,
        color,
        spherical: true,
        ..Spec::default()
    })
}

/// A video stream to check: stream `index` of `path` (`None` for the
/// file's best), called `name` in messages.
struct Stream {
    path: PathBuf,
    index: Option<usize>,
    name: String,
}

impl Stream {
    fn clip(path: PathBuf) -> Self {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        Stream {
            path,
            index: None,
            name,
        }
    }
}

const EXTENSIONS: &[&str] = &["insv", "lrv", "mp4", "mov", "360", "osv"];

/// Camera files under the samples directory, sorted, skipping sidecars
/// such as `.thumbnail.bin`; the test skips without any.
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

/// Every video stream of every file in `files`, and the files that
/// didn't probe.
fn video_streams(files: &[PathBuf]) -> (Vec<Stream>, Vec<String>) {
    let mut streams = Vec::new();
    let mut problems = Vec::new();
    for p in files {
        match probe::probe(p) {
            Ok(r) => {
                for s in r.streams.iter().filter(|s| s.kind == StreamKind::Video) {
                    streams.push(Stream {
                        path: p.clone(),
                        index: Some(s.index),
                        name: format!("{} stream {}", p.display(), s.index),
                    });
                }
            }
            Err(e) => problems.push(format!("{}: probe failed: {e}", p.display())),
        }
    }
    (streams, problems)
}

/// A stream's codec and profile, e.g. "hevc (Main 10)", and its size and
/// level, e.g. "3840x3840 at level 153" (FFmpeg's numbering of levels, as
/// in its own lines).
fn stream_format(s: &Stream) -> (String, String) {
    let Ok(r) = probe::probe(&s.path) else {
        return ("a stream that didn't probe".into(), "?".into());
    };
    let info = r.streams.iter().find(|i| match s.index {
        Some(index) => i.index == index,
        None => i.kind == StreamKind::Video,
    });
    let mut format = info
        .and_then(|s| s.codec.clone())
        .unwrap_or_else(|| "unknown codec".into());
    if let Some(p) = info.and_then(|s| s.profile.as_ref()) {
        format += &format!(" ({p})");
    }
    let mut size = info
        .and_then(|s| s.video.as_ref())
        .map_or("?".into(), |v| format!("{}x{}", v.width, v.height));
    if let Some(level) = info.and_then(|s| s.level) {
        size += &format!(" at level {level}");
    }
    (format, size)
}

/// A backend that decodes on its own device of kind `kind`, and never
/// falls back.
struct Hw {
    backend: FfmpegBackend,
    kind: HwKind,
    /// The device, and why that one, for messages.
    about: String,
    /// Tier 1 only: software decodes in the device's place, labelled as
    /// on it, and fails as this says (media's `fault-injection`).
    stand_in: Option<FaultPlan>,
}

impl Hw {
    fn open(&self, s: &Stream) -> Result<Box<dyn VideoDecode>, MediaError> {
        let req = VideoRequest {
            stream: s.index,
            decode: DecodeMode::Hardware {
                kind: self.kind,
                device: None,
                fallback: Fallback::Fail,
            },
            fault: self.stand_in,
            ..VideoRequest::default()
        };
        self.backend.open_video(&s.path, &req)
    }

    /// [`open`](Self::open), handing frames out as `output` (slice 5b).
    fn open_as(&self, s: &Stream, output: HwOutput) -> Result<Box<dyn VideoDecode>, MediaError> {
        let req = VideoRequest {
            stream: s.index,
            decode: DecodeMode::Hardware {
                kind: self.kind,
                device: None,
                fallback: Fallback::Fail,
            },
            output,
            ..VideoRequest::default()
        };
        self.backend.open_video(&s.path, &req)
    }
}

fn software(s: &Stream) -> Result<Box<dyn VideoDecode>, MediaError> {
    let req = VideoRequest {
        stream: s.index,
        ..VideoRequest::default()
    };
    FfmpegBackend::new().open_video(&s.path, &req)
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
        stand_in: None,
    }
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

/// The backend's Vulkan device, on the GPU behind the render node under
/// test, else the first GPU. `open_vulkan_device` says which GPU that is,
/// or that there is none; the backend must then open it too.
fn vulkan_gpu() -> Result<Hw, NoVulkan> {
    let node = tier2::render_node();
    let about = match open_vulkan_device(node.as_deref()) {
        Ok(c) if c.used.is_cpu() => {
            return Err(NoVulkan::Missing(format!(
                "no GPU for Vulkan Video: FFmpeg's Vulkan device is {c}"
            )));
        }
        Ok(c) => c.to_string(),
        Err(
            e @ MediaError::Hardware {
                reason: HwError::Vulkan(_),
                ..
            },
        ) => return Err(NoVulkan::Broken(e.to_string())),
        Err(e) => {
            return Err(NoVulkan::Missing(format!(
                "no Vulkan device for FFmpeg: {e}"
            )));
        }
    };
    let backend = match node {
        Some(node) => FfmpegBackend::on_render_node(node),
        None => FfmpegBackend::new(),
    };
    if let Err(e) = backend.device(HwKind::Vulkan) {
        return Err(NoVulkan::Broken(format!(
            "FfmpegBackend opened no Vulkan device, where open_vulkan_device found {about}: {e}"
        )));
    }
    eprintln!("Vulkan device: {about}");
    Ok(Hw {
        backend,
        kind: HwKind::Vulkan,
        about: format!("Vulkan on {about}"),
        stand_in: None,
    })
}

/// [`vulkan_gpu`] in a test, which skips without one.
fn vulkan() -> Hw {
    tier2::require_hw_run();
    match vulkan_gpu() {
        Ok(v) => v,
        Err(NoVulkan::Missing(why)) => tier2::skip(why),
        Err(NoVulkan::Broken(why)) => panic!("{why}"),
    }
}

/// Why `hw`'s device turns stream `s` down, if it does, with what FFmpeg
/// logged meanwhile: only the log says why for a driver without the codec
/// or the profile ("Device does not support decoding hevc!"). Any other
/// error is `Err`.
fn turned_down(hw: &Hw, s: &Stream) -> Result<Option<String>, String> {
    let (r, log) = capture_hw_decode_log(|| {
        hw.open(s)
            .and_then(|mut d| d.next_frame(&StopToken::never()))
            .map(drop)
    });
    let ffmpeg = if log.is_empty() {
        String::new()
    } else {
        format!(" (FFmpeg: {})", log.join(" "))
    };
    match r {
        Ok(()) => Ok(None),
        Err(MediaError::HwUnsupported { refusal, .. }) => Ok(Some(format!("{refusal}{ffmpeg}"))),
        Err(e) => Err(format!("{e}{ffmpeg}")),
    }
}

/// Run `check` on every stream in `streams` on `hw`'s device, and fail
/// with every problem found, `problems` first. A stream the device turns
/// down is a problem on VA-API. On Vulkan it is left out, and once the
/// others have passed, the test skips with the reasons, by format.
fn check_all(
    hw: &Hw,
    streams: &[Stream],
    mut problems: Vec<String>,
    check: impl Fn(&Hw, &Stream) -> Result<Vec<String>, String>,
) {
    // The streams turned down, by format and reason: their sizes and how
    // many.
    let mut refused: BTreeMap<(String, String), (BTreeSet<String>, usize)> = BTreeMap::new();
    let mut checked = 0;
    for s in streams {
        eprintln!("{}: on {}", s.name, hw.about);
        match turned_down(hw, s) {
            Ok(None) => {}
            Ok(Some(why)) if hw.kind == HwKind::Vulkan => {
                let (format, size) = stream_format(s);
                let e = refused.entry((format, why)).or_default();
                e.0.insert(size);
                e.1 += 1;
                continue;
            }
            Ok(Some(why)) => {
                problems.push(format!("{}: {} turned it down: {why}", s.name, hw.about));
                continue;
            }
            Err(e) => {
                problems.push(format!("{}: on {}: {e}", s.name, hw.about));
                continue;
            }
        }
        checked += 1;
        match check(hw, s) {
            Ok(p) => problems.extend(p.into_iter().map(|p| format!("{}: {p}", s.name))),
            Err(e) => problems.push(format!("{}: {e}", s.name)),
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
        "on {}:\n{}{}",
        hw.about,
        problems.join("\n"),
        if refused.is_empty() {
            String::new()
        } else {
            format!("\nVulkan Video can't decode: {refusals}")
        }
    );
    // Never a pass that compared nothing.
    assert!(
        checked > 0 || !refused.is_empty(),
        "no video stream to check"
    );
    if !refused.is_empty() {
        let n: usize = refused.values().map(|(_, n)| n).sum();
        tier2::skip(format!(
            "Vulkan Video can't decode {n} of {} streams on {}; {refusals}{}",
            n + checked,
            hw.about,
            if checked > 0 {
                format!("; the other {checked} match software")
            } else {
                String::new()
            }
        ));
    }
}

/// FNV-1a of each channel of `f` (Y, U and V, or G, B and R), over its
/// samples as values of the layout's bit depth, so that NV12 hashes as
/// yuv420p does and P010 as yuv420p10le.
fn channel_hashes(f: &VideoFrame) -> Vec<u64> {
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    let fnv = |h: &mut u64, v: u16| {
        for b in v.to_le_bytes() {
            *h = (*h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let layout = f.layout().clone();
    let shift = if layout.msb_aligned {
        16 - u32::from(layout.bit_depth)
    } else {
        0
    };
    let mut out = Vec::new();
    for (i, desc) in layout.planes.iter().enumerate() {
        let p = f.plane(i).unwrap();
        let row = desc.row_bytes(p.width);
        let n = usize::from(desc.components);
        let mut hashes = vec![BASIS; n];
        for y in 0..p.height {
            let r = p.row(y, row).unwrap();
            match desc.bytes {
                SampleBytes::One => {
                    for (x, &b) in r.iter().enumerate() {
                        fnv(&mut hashes[x % n], u16::from(b));
                    }
                }
                SampleBytes::Two => {
                    for (x, s) in r.chunks_exact(2).enumerate() {
                        fnv(
                            &mut hashes[x % n],
                            u16::from_le_bytes([s[0], s[1]]) >> shift,
                        );
                    }
                }
            }
        }
        out.extend(hashes);
    }
    out
}

/// How frame `a`, from `on`, differs from software's `b`, if it does.
fn differs(on: &str, a: &VideoFrame, b: &VideoFrame) -> Option<String> {
    if a.pts != b.pts {
        return Some(format!("{on} gave pts {}, software pts {}", a.pts, b.pts));
    }
    if a.size() != b.size() {
        return Some(format!(
            "pts {}: {on}'s frame is {:?}, software's {:?}",
            a.pts,
            a.size(),
            b.size()
        ));
    }
    let (ha, hb) = (channel_hashes(a), channel_hashes(b));
    if ha.len() != hb.len() {
        return Some(format!(
            "pts {}: {on}'s {} has {} channels, software's {} {}",
            a.pts,
            a.layout().name,
            ha.len(),
            b.layout().name,
            hb.len()
        ));
    }
    let bad: Vec<usize> = (0..ha.len()).filter(|&c| ha[c] != hb[c]).collect();
    (!bad.is_empty()).then(|| {
        format!(
            "pts {}: channels {bad:?} of {on}'s {} differ from software's {}",
            a.pts,
            a.layout().name,
            b.layout().name
        )
    })
}

fn seek(
    d: &mut dyn VideoDecode,
    t: Time,
    mode: SeekMode,
) -> Result<(std::sync::Arc<VideoFrame>, Exactness), MediaError> {
    match d.seek(t, mode, &StopToken::never())? {
        SeekOutcome::Frame {
            frame, exactness, ..
        } => Ok((frame, exactness)),
        SeekOutcome::Stopped => panic!("stopped without a stop"),
    }
}

fn next(d: &mut dyn VideoDecode) -> Result<Option<std::sync::Arc<VideoFrame>>, MediaError> {
    match d.next_frame(&StopToken::never())? {
        NextFrame::Frame(f) => Ok(Some(f)),
        NextFrame::End => Ok(None),
        NextFrame::Stopped => panic!("stopped without a stop"),
    }
}

/// The frame shown at pts `t`, by its place in output order: the one with
/// the greatest pts at or before `t`, the later of frames that tie.
fn shown_at(pts: &[i64], t: i64) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, &p) in pts.iter().enumerate() {
        if p <= t && best.is_none_or(|b| p >= pts[b]) {
            best = Some(i);
        }
    }
    best
}

/// A small deterministic generator for the random targets.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

/// The stream's length in time-base ticks from its first frame.
fn length_ticks(d: &dyn VideoDecode) -> Result<i64, String> {
    let info = d.info();
    let duration = info.duration.ok_or("the stream has no duration")?;
    duration
        .to_pts(info.time_base, Rounding::Floor)
        .map_err(|e| format!("its duration: {e}"))
}

/// Where the damage among `events` was: after which pts.
fn damage(events: Vec<DecoderEvent>) -> Vec<Option<i64>> {
    events
        .into_iter()
        .filter_map(|e| match e {
            DecoderEvent::Damaged { last_good_pts, .. } => Some(last_good_pts),
            _ => None,
        })
        .collect()
}

/// Decode `s` from start to end on the device for every frame's pts, then
/// seek it and software to [`SEEKS`] random times ([`SEEKS_LARGE`] above
/// 2880²), and to a keyframe
/// after every tenth: each seek must return the frame the sequential
/// decode shows then, software's bit for bit, and so must the frame after
/// it.
fn check_seeks(hw: &Hw, s: &Stream) -> Result<Vec<String>, String> {
    let on = hw.about.as_str();
    let mut dev = hw.open(s).map_err(|e| format!("opening on {on}: {e}"))?;
    let mut seq = Vec::new();
    while let Some(f) = next(&mut *dev)
        .map_err(|e| format!("frame {} of a sequential decode on {on}: {e}", seq.len()))?
    {
        seq.push(f.pts);
    }
    if seq.is_empty() {
        return Err(format!("a sequential decode on {on} gave no frames"));
    }
    let mut problems = Vec::new();
    let seq_damage = damage(dev.take_events());
    if !seq_damage.is_empty() {
        problems.push(format!(
            "a sequential decode on {on} reported damage after {seq_damage:?}"
        ));
    }
    let info = dev.info().clone();
    let ticks = length_ticks(&*dev)?;
    let mut sw = software(s).map_err(|e| format!("opening in software: {e}"))?;
    let path_ok = DecodePath::Hardware { kind: hw.kind };
    let mut rng = Rng::new(0x9e37_79b9_7f4a_7c15 ^ s.index.unwrap_or(0) as u64);
    let (w, h) = info.size;
    let seeks = if u64::from(w) * u64::from(h) > 2880 * 2880 {
        SEEKS_LARGE
    } else {
        SEEKS
    };
    for i in 0..seeks {
        if problems.len() >= MAX_PROBLEMS {
            problems.push(format!("stopped after {MAX_PROBLEMS} problems"));
            break;
        }
        let offset = rng.below(ticks.max(1) as u64) as i64;
        let pts = info.origin_pts + offset;
        let t = Time::from_pts(offset, info.time_base).map_err(|e| format!("pts {pts}: {e}"))?;
        let what = format!("exact seek {i} to pts {pts}");
        let (a, exactness) =
            seek(&mut *dev, t, SeekMode::Exact).map_err(|e| format!("{what} on {on}: {e}"))?;
        let (b, _) =
            seek(&mut *sw, t, SeekMode::Exact).map_err(|e| format!("{what} in software: {e}"))?;
        let want = shown_at(&seq, pts).map(|k| seq[k]);
        if (Some(a.pts), exactness) != (want, Exactness::Exact) {
            problems.push(format!(
                "{what}: {on} gave pts {} ({exactness:?}); the sequential decode shows pts \
                 {want:?} then",
                a.pts
            ));
        }
        if a.decoded_on != path_ok {
            problems.push(format!("{what}: decoded {:?}", a.decoded_on));
        }
        if let Some(p) = differs(on, &a, &b) {
            problems.push(format!("{what}: {p}"));
        }
        match (
            next(&mut *dev).map_err(|e| format!("the frame after {what} on {on}: {e}"))?,
            next(&mut *sw).map_err(|e| format!("the frame after {what} in software: {e}"))?,
        ) {
            (Some(a), Some(b)) => {
                if let Some(p) = differs(on, &a, &b) {
                    problems.push(format!("the frame after {what}: {p}"));
                }
            }
            (None, None) => {}
            (a, b) => problems.push(format!(
                "the frame after {what}: {on} gave {:?}, software {:?}",
                a.map(|f| f.pts),
                b.map(|f| f.pts)
            )),
        }
        // A keyframe seek lands where software's does, and leaves the
        // decoder where the next exact seek can decode forward from.
        if i % 10 == 9 {
            let what = format!("keyframe seek to pts {pts}");
            let (a, ea) = seek(&mut *dev, t, SeekMode::Keyframe)
                .map_err(|e| format!("{what} on {on}: {e}"))?;
            let (b, eb) = seek(&mut *sw, t, SeekMode::Keyframe)
                .map_err(|e| format!("{what} in software: {e}"))?;
            if ea != eb || !seq.contains(&a.pts) {
                problems.push(format!(
                    "{what}: {on} gave pts {} ({ea:?}), software pts {} ({eb:?})",
                    a.pts, b.pts
                ));
            }
            if let Some(p) = differs(on, &a, &b) {
                problems.push(format!("{what}: {p}"));
            }
        }
    }
    let (da, db) = (damage(dev.take_events()), damage(sw.take_events()));
    if da != db {
        problems.push(format!(
            "damage reported while seeking: on {on} after {da:?}, in software after {db:?}"
        ));
    }
    if *dev.decode_path() != path_ok {
        problems.push(format!("the decoder ended {:?}", dev.decode_path()));
    }
    Ok(problems)
}

/// The noisy clips, four cropped ones and every sample stream, and the
/// samples that didn't probe. The samples are listed first, so that a
/// machine without them skips before generating the clips.
fn seek_streams() -> (Vec<Stream>, Vec<String>) {
    let (samples, problems) = video_streams(&sample_files());
    let mut streams: Vec<Stream> = [
        noisy(8),
        noisy(10),
        cropped(Codec::H264),
        cropped(Codec::Hevc),
        left_cropped(Codec::H264, 8),
        left_cropped(Codec::Hevc, 10),
    ]
    .into_iter()
    .map(Stream::clip)
    .collect();
    streams.extend(samples);
    (streams, problems)
}

#[test]
#[ignore = "tier2: needs a VA-API GPU and camera samples"]
fn vaapi_seek_matches_software() {
    let hw = vaapi();
    let (streams, problems) = seek_streams();
    check_all(&hw, &streams, problems, check_seeks);
}

/// Frames a stream's exported frames are checked against downloads for.
const EXPORTED_FRAMES: usize = 8;

/// Exported (slice 5b), `s`'s first frames are a download's: each one,
/// copied back with `to_system`, has the downloaded frame's channels, pts
/// and size, and its layers are its planes, each its own fd's. A frame
/// that isn't exported is a problem, with the reason: VA-API on odin and
/// huginn exports every X5 mode. The noisy clips cropped on the left are
/// the exception: they are downloaded, and say so.
fn check_export(hw: &Hw, s: &Stream) -> Result<Vec<String>, String> {
    let on = hw.about.as_str();
    let mut ex = hw
        .open_as(s, HwOutput::DrmPrime)
        .map_err(|e| format!("opening on {on} to export: {e}"))?;
    let mut down = hw
        .open_as(s, HwOutput::Download)
        .map_err(|e| format!("opening on {on} to download: {e}"))?;
    let left_cropped = s.name.contains("left");
    let mut problems = Vec::new();
    for i in 0..EXPORTED_FRAMES {
        let (Some(a), Some(b)) = (
            next(&mut *ex).map_err(|e| format!("exported frame {i}: {e}"))?,
            next(&mut *down).map_err(|e| format!("downloaded frame {i}: {e}"))?,
        ) else {
            break;
        };
        match (&a.transfer, left_cropped) {
            (Transfer::DrmPrime, false) => {}
            (
                Transfer::Downloaded {
                    not_exported: Some(why),
                },
                true,
            ) if why.contains("cropped on the left or top") => {
                continue;
            }
            (t, _) => {
                problems.push(format!("frame {i} on {on} left the decoder as {t:?}"));
                continue;
            }
        }
        let layers = a.dmabuf().expect("an exported frame's layers");
        if layers.len() != a.layout().planes.len() {
            problems.push(format!("frame {i}: {} layers", layers.len()));
        }
        if a.plane(0).is_some() {
            problems.push(format!("frame {i}: an exported frame gave its bytes"));
        }
        let back = a
            .to_system()
            .expect("a frame on the device")
            .map_err(|e| format!("frame {i} back from the device: {e}"))?;
        if (back.pts, back.size()) != (b.pts, b.size()) {
            problems.push(format!(
                "frame {i}: exported {} {:?}, downloaded {} {:?}",
                back.pts,
                back.size(),
                b.pts,
                b.size()
            ));
        } else if channel_hashes(&back) != channel_hashes(&b) {
            problems.push(format!(
                "frame {i} (pts {}): exported and downloaded differ",
                b.pts
            ));
        }
    }
    Ok(problems)
}

#[test]
#[ignore = "tier2: needs a VA-API GPU and camera samples"]
fn vaapi_exports_match_downloads() {
    let hw = vaapi();
    let (streams, problems) = seek_streams();
    check_all(&hw, &streams, problems, check_export);
}

#[test]
#[ignore = "tier2: needs Vulkan Video decode (optional path) and camera samples"]
fn vulkan_seek_matches_software() {
    let hw = vulkan();
    let (streams, problems) = seek_streams();
    check_all(&hw, &streams, problems, check_seeks);
}

/// The colour `s`'s stream and frames resolve to (D30's table) on the
/// device must be software's, field for field, assumed flags included:
/// the stream's, its first three frames' and a frame from the middle.
fn check_color(hw: &Hw, s: &Stream) -> Result<Vec<String>, String> {
    let on = hw.about.as_str();
    let mut dev = hw.open(s).map_err(|e| format!("opening on {on}: {e}"))?;
    let mut sw = software(s).map_err(|e| format!("opening in software: {e}"))?;
    let mut problems = Vec::new();
    let (a, b) = (dev.info(), sw.info());
    if a.color != b.color {
        problems.push(format!(
            "the stream's colour on {on} ({}) is {:?}, in software ({}) {:?}",
            a.source_pix_fmt, a.color, b.source_pix_fmt, b.color
        ));
    }
    if a.size != b.size {
        problems.push(format!(
            "the stream is {:?} on {on}, {:?} in software",
            a.size, b.size
        ));
    }
    let mut frames = Vec::new();
    for _ in 0..3 {
        match (
            next(&mut *dev).map_err(|e| format!("decoding on {on}: {e}"))?,
            next(&mut *sw).map_err(|e| format!("decoding in software: {e}"))?,
        ) {
            (Some(a), Some(b)) => frames.push((a, b)),
            (None, None) => break,
            (a, b) => {
                return Err(format!(
                    "{on} gave {:?}, software {:?}",
                    a.map(|f| f.pts),
                    b.map(|f| f.pts)
                ));
            }
        }
    }
    let half = Time::from_pts(length_ticks(&*dev)? / 2, dev.info().time_base)
        .map_err(|e| format!("the middle: {e}"))?;
    let (a, _) =
        seek(&mut *dev, half, SeekMode::Exact).map_err(|e| format!("seeking on {on}: {e}"))?;
    let (b, _) =
        seek(&mut *sw, half, SeekMode::Exact).map_err(|e| format!("seeking in software: {e}"))?;
    frames.push((a, b));
    for (a, b) in &frames {
        if a.color() != b.color() {
            problems.push(format!(
                "the frame at pts {} on {on} has colour {:?}, in software (pts {}) {:?}",
                a.pts,
                a.color(),
                b.pts,
                b.color()
            ));
        }
    }
    Ok(problems)
}

/// Clips tagged every way the default rules care about, the noisy clips
/// and every sample stream (the X5's LRV is full range).
fn color_streams() -> (Vec<Stream>, Vec<String>) {
    let (samples, problems) = video_streams(&sample_files());
    let full = ColorTags {
        range: 2,
        ..CLIP_COLOR
    };
    let bt601 = ColorTags {
        matrix: 6,
        primaries: 6,
        transfer: 6,
        ..CLIP_COLOR
    };
    // HDR10's tags, with the left siting its 4:2:0 has.
    let pq = ColorTags {
        range: 1,
        matrix: 9,
        transfer: 16,
        primaries: 9,
        chroma_location: 1,
    };
    let hlg = ColorTags { transfer: 18, ..pq };
    let mut streams: Vec<Stream> = [
        tagged("t2-color-h264-bt709", Codec::H264, 8, 1024, CLIP_COLOR),
        // An untagged matrix is assumed BT.709 from 1280 wide, BT.601
        // below.
        tagged("t2-color-h264-none", Codec::H264, 8, 1280, ColorTags::NONE),
        tagged(
            "t2-color-h264-none-sd",
            Codec::H264,
            8,
            640,
            ColorTags::NONE,
        ),
        tagged("t2-color-h264-full", Codec::H264, 8, 1024, full),
        tagged("t2-color-hevc-bt601", Codec::Hevc, 8, 1024, bt601),
        tagged(
            "t2-color-hevc10-none",
            Codec::Hevc,
            10,
            1280,
            ColorTags::NONE,
        ),
        tagged("t2-color-hevc10-pq", Codec::Hevc, 10, 1024, pq),
        tagged("t2-color-hevc10-hlg", Codec::Hevc, 10, 1024, hlg),
        // Their size on the device must be software's too.
        left_cropped(Codec::H264, 8),
        left_cropped(Codec::Hevc, 10),
        noisy(8),
        noisy(10),
    ]
    .into_iter()
    .map(Stream::clip)
    .collect();
    streams.extend(samples);
    (streams, problems)
}

#[test]
#[ignore = "tier2: needs a VA-API GPU and camera samples"]
fn vaapi_colorinfo_matches_software() {
    let hw = vaapi();
    let (streams, problems) = color_streams();
    check_all(&hw, &streams, problems, check_color);
}

#[test]
#[ignore = "tier2: needs Vulkan Video decode (optional path) and camera samples"]
fn vulkan_colorinfo_matches_software() {
    let hw = vulkan();
    let (streams, problems) = color_streams();
    check_all(&hw, &streams, problems, check_color);
}

/// Seeks in one batch of [`bench::try_measure_latency`].
const BATCH: usize = 20;

/// The milliseconds each of [`BATCH`] seeks in `mode` to random times
/// took to hand out its frame, on a decoder `open` makes for the batch.
/// Every batch seeks to the same times.
fn seek_batch(
    open: &dyn Fn() -> Result<Box<dyn VideoDecode>, MediaError>,
    mode: SeekMode,
) -> Result<Vec<f64>, String> {
    let mut d = open().map_err(|e| e.to_string())?;
    let ticks = length_ticks(&*d)?;
    let tb = d.info().time_base;
    let mut rng = Rng::new(0x2545_f491_4f6c_dd1d);
    let mut out = Vec::with_capacity(BATCH);
    for _ in 0..BATCH {
        let t =
            Time::from_pts(rng.below(ticks.max(1) as u64) as i64, tb).map_err(|e| e.to_string())?;
        let started = Instant::now();
        seek(&mut *d, t, mode).map_err(|e| e.to_string())?;
        out.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(out)
}

/// The sample of `extension` whose first video stream is at least
/// `min_width` wide and lasts longest, and what its rows call it, e.g.
/// "3840x3840 hevc at 29.97 fps (stream 0 of VID_…_009.insv)".
fn longest_sample(files: &[PathBuf], extension: &str, min_width: u32) -> Option<(String, Stream)> {
    let mut best: Option<(f64, String, Stream)> = None;
    for p in files {
        if !p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case(extension))
        {
            continue;
        }
        let Ok(r) = probe::probe(p) else { continue };
        let Some(v) = r.streams.iter().find(|s| s.kind == StreamKind::Video) else {
            continue;
        };
        let Some(vi) = v.video.as_ref().filter(|vi| vi.width >= min_width) else {
            continue;
        };
        let len = v.duration.map_or(0.0, |d| d.to_secs_f64());
        if best.as_ref().is_some_and(|(l, ..)| *l >= len) {
            continue;
        }
        let camera = r
            .camera
            .as_ref()
            .and_then(|c| c.model.as_ref())
            .map(|m| format!("{m} "))
            .unwrap_or_default();
        let label = format!(
            "{camera}{}x{} {} at {:.2} fps (stream {} of {})",
            vi.width,
            vi.height,
            v.codec.as_deref().unwrap_or("?"),
            v.avg_frame_rate.map_or(0.0, |r| r.to_f64()),
            v.index,
            p.file_name().unwrap_or_default().to_string_lossy()
        );
        let s = Stream {
            path: p.clone(),
            index: Some(v.index),
            name: label.clone(),
        };
        best = Some((len, label, s));
    }
    best.map(|(_, label, s)| (label, s))
}

#[test]
#[ignore = "tier2: benchmark, needs a VA-API GPU and camera samples"]
fn bench_seek_latency() {
    let va = vaapi();
    let files = sample_files();
    // Vulkan Video is optional here: without it, or where it can't decode
    // a clip, its rows are left out and the report says why.
    let vk = vulkan_gpu();
    if let Ok(vk) = &vk {
        bench::note(format!("Vulkan Video rows ran on {}", vk.about));
    }
    let mut clips = vec![(
        "noisy 5760x2880 HEVC 8-bit".to_string(),
        Some(Stream::clip(noisy(8))),
    )];
    for (what, extension, min_width) in [("an 8K .insv", "insv", 3840), ("an .lrv", "lrv", 0)] {
        match longest_sample(&files, extension, min_width) {
            Some((label, s)) => clips.push((label, Some(s))),
            None => clips.push((format!("{what} sample"), None)),
        }
    }
    for (label, stream) in &clips {
        for (mode, mode_name) in [(SeekMode::Exact, "exact"), (SeekMode::Keyframe, "keyframe")] {
            let row = |on: &str| format!("{mode_name} seek, {label}, {on}");
            let Some(s) = stream else {
                for on in ["software", "VA-API", "Vulkan Video"] {
                    bench::not_measured(&row(on), "no such sample");
                }
                continue;
            };
            bench::try_measure_latency(&row("software"), "ms", || {
                seek_batch(&|| software(s), mode)
            })
            .unwrap_or_else(|e| panic!("{}: software: {e}", s.name));
            bench::try_measure_latency(&row("VA-API"), "ms", || seek_batch(&|| va.open(s), mode))
                .unwrap_or_else(|e| panic!("{}: {}: {e}", s.name, va.about));
            let measured = match &vk {
                Err(no) => Err(no.reason().to_string()),
                Ok(vk) => match turned_down(vk, s) {
                    Ok(None) => bench::try_measure_latency(&row("Vulkan Video"), "ms", || {
                        seek_batch(&|| vk.open(s), mode)
                    }),
                    Ok(Some(why)) => Err(why),
                    Err(e) => Err(e),
                },
            };
            if let Err(why) = measured {
                bench::not_measured(&row("Vulkan Video"), why);
            }
        }
    }
}

/// A test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tier2-backend-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Software standing in for VA-API, failing frame `fail_at` (by the order
/// the decoder makes frames in) as decoding on the device would.
fn stand_in(fail_at: Option<u64>) -> Hw {
    Hw {
        backend: FfmpegBackend::new(),
        kind: HwKind::VaApi,
        about: "a software stand-in for VA-API".into(),
        stand_in: Some(FaultPlan {
            frame: fail_at.unwrap_or(u64::MAX),
            fault: Fault::Decode,
            stand_in: true,
        }),
    }
}

/// Clips of 30 frames in open GOPs of 8 with 3 B-frames, in `dir`: 256x128
/// H.264 at 8 bits and HEVC at 10, 250x142 H.264, coded as 256x144,
/// which the decoder crops as it does [`cropped`]'s, and HEVC at 10 bits
/// cropped 48 columns on the left as [`left_cropped`]'s is.
fn small_clips(dir: &TempDir) -> Vec<Stream> {
    [
        (Codec::H264, 8, 256, 128, 0),
        (Codec::Hevc, 10, 256, 128, 0),
        (Codec::H264, 8, 250, 142, 0),
        (Codec::Hevc, 10, 256, 128, 48),
    ]
    .into_iter()
    .map(|(codec, bits, width, height, crop_left)| {
        let spec = Spec {
            name: format!("{codec:?}-{bits}-{width}-{crop_left}"),
            codec,
            bits,
            width,
            height,
            crop_left,
            gop: Some(Gop {
                keyint: 8,
                bframes: 3,
                open: true,
                radl: 0,
            }),
            ..Spec::default()
        };
        Stream::clip(media::write(&spec, &dir.0).unwrap())
    })
    .collect()
}

/// The Tier 2 checks must pass where the "device" decodes as software
/// does, so that a failure on a GPU is the GPU's, not the checks'.
#[test]
fn the_checks_pass_on_software_standing_in_for_a_device() {
    let dir = TempDir::new("pass");
    let streams = small_clips(&dir);
    let hw = stand_in(None);
    check_all(&hw, &streams, Vec::new(), check_seeks);
    check_all(&hw, &streams, Vec::new(), check_color);
}

/// And they must fail where it fails: in the sequential decode, in a
/// seek, and where it can't open the stream at all.
#[test]
fn the_checks_fail_on_a_device_that_fails() {
    let dir = TempDir::new("fail");
    let streams = small_clips(&dir);
    // 30 frames in the sequential decode, then the seeks'.
    for k in [5, 45] {
        for s in &streams {
            let e = check_seeks(&stand_in(Some(k)), s).unwrap_err();
            assert!(e.contains("fault injected"), "frame {k}: {e}");
        }
    }
    let e = check_color(&stand_in(Some(0)), &streams[0]).unwrap_err();
    assert!(e.contains("fault injected"), "{e}");
    let e = turned_down(&stand_in(Some(0)), &streams[0]).unwrap_err();
    assert!(e.contains("fault injected"), "{e}");
}

/// NV12 hashes as yuv420p does and P010 as yuv420p10le, channel by
/// channel, and one sample off changes its own channel's hash only.
#[test]
fn channels_hash_alike_in_every_layout_and_apart_where_a_sample_differs() {
    use spherecut_core::video::{ColorCodes, ColorFamily, PixelLayout, PlaneDesc};
    use spherecut_media::color::{self, ColorContext};
    use std::sync::Arc;

    let layout = |name: &str, bits: u8, semi: bool| {
        let bytes = if bits > 8 {
            SampleBytes::Two
        } else {
            SampleBytes::One
        };
        let plane = |components, sub| PlaneDesc {
            components,
            bytes,
            log2_sub: (sub, sub),
        };
        Arc::new(PixelLayout {
            name: name.into(),
            planes: if semi {
                vec![plane(1, 0), plane(2, 1)]
            } else {
                vec![plane(1, 0), plane(1, 1), plane(1, 1)]
            },
            family: ColorFamily::Yuv,
            bit_depth: bits,
            msb_aligned: semi && bits > 8,
        })
    };
    let color = color::resolve(
        ColorCodes::UNSPECIFIED,
        &ColorContext {
            family: ColorFamily::Yuv,
            chroma_sub: (1, 1),
            pix_fmt: "yuv420p",
            codec: "h264",
            width: 4,
            height: 2,
        },
    );
    // A 4x2 picture: 8 luma samples, and 2 of each chroma.
    let (y, u, v): (Vec<u16>, Vec<u16>, Vec<u16>) = (
        (0..8).map(|i| 64 + 37 * i).collect(),
        vec![300, 301],
        vec![700, 702],
    );
    let frame = |bits: u8, semi: bool, y: &[u16], u: &[u16], v: &[u16]| {
        let name = match (semi, bits > 8) {
            (true, true) => "p010le",
            (true, false) => "nv12",
            (false, true) => "yuv420p10le",
            (false, false) => "yuv420p",
        };
        // At 8 bits the samples are taken modulo 256; P010 keeps them in
        // the high bits.
        let bytes = |s: &[u16]| -> Vec<u8> {
            match (bits > 8, semi) {
                (true, true) => s.iter().flat_map(|v| (v << 6).to_le_bytes()).collect(),
                (true, false) => s.iter().flat_map(|v| v.to_le_bytes()).collect(),
                (false, _) => s.iter().map(|&v| v as u8).collect(),
            }
        };
        let width = if bits > 8 { 2 } else { 1 };
        let (planes, strides) = if semi {
            let uv: Vec<u16> = u.iter().zip(v).flat_map(|(&a, &b)| [a, b]).collect();
            (vec![bytes(y), bytes(&uv)], vec![4 * width, 4 * width])
        } else {
            (
                vec![bytes(y), bytes(u), bytes(v)],
                vec![4 * width, 2 * width, 2 * width],
            )
        };
        VideoFrame::owned(
            layout(name, bits, semi),
            (4, 2),
            planes,
            strides,
            color,
            (0, Time::ZERO),
        )
        .unwrap()
    };
    for bits in [8, 10] {
        let planar = frame(bits, false, &y, &u, &v);
        let semi = frame(bits, true, &y, &u, &v);
        assert_eq!(
            channel_hashes(&planar),
            channel_hashes(&semi),
            "{bits} bits"
        );
        assert_eq!(differs("the device", &semi, &planar), None);
        let mut v2 = v.clone();
        v2[1] += 1;
        let off = frame(bits, true, &y, &u, &v2);
        let (a, b) = (channel_hashes(&off), channel_hashes(&planar));
        assert_eq!(
            (a[0] == b[0], a[1] == b[1], a[2] == b[2]),
            (true, true, false)
        );
        let why = differs("the device", &off, &planar).unwrap();
        assert!(why.contains("channels [2]"), "{why}");
    }
}
