# Spherecut M1 (Viewer): the implementation plan

*2026-10-24, before M1's code, at 07f3ef1. Written from three designs
(performance, simplicity, correctness) and two judges' verdicts, and checked
against the repo, PLAN.md M1, docs/PROMPT.md, DECISIONS.md D1 to D23, the
crate sources in the local cargo registry, and huginn's report in PR #2.*


## 0. Amendments, 2026-10-24 (read first)

These changes came after the plan was written. Where they conflict with
the sections below, they win.

1. **DECISIONS numbering.** D24 is FFmpeg confinement, as planned. D25
   (the zero-copy proof) and D26 (the egui check at the end of M2) were
   taken by Pat's approval of the stack review. The entries this plan
   calls D25 to D35 are recorded as **D27 to D37**: the CLI JSON contract
   D27, the crate layout D28, MediaBackend and seek D29, pixel formats,
   colour and fallback D30, the render pipeline D31, the golden policy
   D32, view conventions D33, the playback engine D34, audio D35, GUI
   dependencies D36, hardware testing in M1 D37.
2. **Zero-copy proof in M1 (D25, PLAN M1).** A new slice **5b** runs
   after slice 5 and before H1:
   - `FrameStorage` gains a GPU-resident variant holding the VA-API
     surface mapped to DRM PRIME (`av_hwframe_map` to
     `AV_PIX_FMT_DRM_PRIME`), with its `AVDRMFrameDescriptor` (fds,
     offsets, pitches, modifiers), keeping the hardware frame alive while
     the GPU uses it. `VideoPlanes` stays the CPU path.
   - `spherecut-render` gets a `dmabuf` module, the first GPU-side
     `unsafe` (moved from M9 to M1; section 2.4's last bullet changes
     accordingly; render goes from `forbid` to `deny(unsafe_code)` with
     an allow on that module only). It imports each plane as its own
     texture (R8/RG8, or R16/RG16) through wgpu-hal's Vulkan backend with
     `VK_EXT_external_memory_dma_buf` and
     `VK_EXT_image_drm_format_modifier`, and wraps it for wgpu. Check the
     wgpu 30 hal API for this before writing it.
   - Tier 2 `zero_copy_matches_copy_path`: NV12 from an X5 sample and
     P010 from a 10-bit clip, one frame rendered both ways, compared.
     Tier 2 `bench_x5_8k30_zero_copy_vs_copy`: X5 8K30 playback both
     ways. Both are in H1. Without the extensions, or when the driver
     refuses the modifier, they skip with the reason (never pass).
   - The overlay, doctor and `spherecut frame --json` say which path a
     frame took. The copy path stays the default until the decision
     before M3.
   - If per-plane import fails on RADV, D25's fallback (one Vulkan device
     we create, given to FFmpeg's Vulkan hwcontext and to wgpu) is
     designed then, not now.
3. **Vulkan Video on odin.** Branch `m1/vulkan-video-tier2` adds Vulkan
   Video HEVC Main/Main10/H.264 bit-exact tests, a Vulkan bit-exact check
   on the X5 samples and Vulkan rows in the decode benchmarks, and updates
   odin's run prompt. odin's first run is requested after it and the
   confinement branch are merged (slice 1).
4. **Agent integration (PLAN track A).** Pat chose A1 after M1's code and
   before M2: A1 starts after slice 8's H2 request, while H2 is pending,
   and slice 9 follows H2. The JSON contract types of slices 1 and 4 are
   written so A1's `spherecut-agent` crate can reuse them, and times
   serialize as `{exact, seconds}` plus `frame` when the time is a frame
   of a video stream.
5. **egui check (D26)** belongs to M2. M1 keeps UI types out of render,
   playback and the project model, as section 2.1 already requires.
6. **Confinement** is implemented on branch `m1/ffmpeg-confine` (with
   D24); where its reviewed design differs from section 3.6, the branch
   wins.
7. **Questions for Pat** went out on 2026-10-24: fonts and screen readers
   on decision cards; clip 12 and Hyprland in the thread. GNOME
   decorations wait for H2's screenshots, and odin's run waits for item 3.

## 1. Summary

### 1.1 Which design this plan starts from

The judges disagreed. Judge 1 ranked performance first (46), then
simplicity (44) and correctness (43). Judge 2 ranked simplicity first
(45.5), then performance (43.5) and correctness (40.5). The totals tie:
performance 89.5, simplicity 89.5, correctness 83.5.

The plan starts from **performance**, for three reasons:

- Both judges scored it above simplicity on correctness (8 and 7.5 against
  7 and 7), performance (9 and 9 against 6 and 6.5) and extensibility (9
  and 8.5 against 6 and 6.5).
- Its gaps are self-contained additions: FFmpeg confinement, the CLI JSON
  contract and the DECISIONS numbering. Both judges list exactly these as
  grafts from simplicity, and they bolt on without touching the rest.
  Simplicity's gaps are structural: the engine lives in `app` behind a
  `Mutex` that the UI locks, `render` depends on `media`, 4:2:2 and 4:4:4
  go through CPU swscale, and the render node is hard-coded. Judge 2's own
  grafts move simplicity toward performance's crate structure.
- The crate structure is what costs most to change later. M2's export,
  `spherecut render` and M5's timeline all drive the playback engine
  without a window.

From **simplicity** it takes:

- the whole confinement design;
- the JSON contract;
- DECISIONS numbering from D24;
- the slice order (confinement first, a headless hardware run before the
  GUI, the odin request in slice 1);
- the criterion-to-slice table.

From **correctness** it takes:

- the measured seek algorithm and its oracle;
- seam and pole tests that put the discontinuity inside a 2×2 quad;
- the f64 CPU trilinear reference;
- integer uploads into canonical f16 planes;
- chroma siting;
- the fake-audio-device A/V tests;
- the `may_block` guard;
- the cgroup-aware cache budget;
- Wayland-socket discovery from a TTY;
- the noisy perf clip;
- Tier 1 on the hardware machines.

It also simplifies performance where both judges called it the most
complex design:

- 4 GPU slots instead of 6;
- one mip shader instead of a render-pass path plus a compute path;
- `Queue::write_texture` as the measured baseline, instead of a
  StagingBelt from day one;
- no `on_submitted_work_done` retirement, because wgpu already orders
  work on one queue.

Appendix A lists every error the judges found and where this plan fixes it.

### 1.2 What M1 builds, in one paragraph per layer

**Media.** `MediaBackend` gains an FFmpeg implementation that opens only
the file Spherecut opened. The file goes through our own AVIO: nested
opens are refused, only MP4/MOV and Matroska demuxers are allowed, and
every decoder has a `max_pixels` limit. Video decodes in software or
through VA-API, with an automatic, visible fallback. Planes arrive in
their native layout: NV12/P010 from VA-API, planar from software. Seeks
are frame-exact, using the re-seek-to-dts−1 algorithm that seekprobe
measured. Audio decodes on the same clip timeline.

**Render.** A new `spherecut-render` crate has no FFmpeg and no egui in it.
It uploads planes per plane:

- 8-bit planes go straight into `R8Unorm`/`Rg8Unorm`.
- Deeper planes go into `R16Uint`/`Rg16Uint`, and an ingest pass turns
  them into canonical `R16Float`/`Rg16Float`.

One render-pass shader builds an area-weighted mip chain, capped near
64 px. The WGSL projection shader projects rectilinear, fisheye and
stereographic views, with morph. It takes analytic gradients from the
view direction, so there is no seam and no shimmer. It honours chroma
siting, writes a linear `Rgba16Float` target, and an encode pass applies
the sRGB curve.

**CLI.** `spherecut frame` renders one PNG headless. probe, doctor and
frame share a JSON envelope and one set of exit codes, 0 to 4 (probe and
doctor use only 0, 1 and 2). Outputs are never
replaced without `--overwrite`.

**Playback.** A new `spherecut-playback` crate has no egui in it. It runs
these on worker threads:

- decode;
- upload;
- audio.

It keeps a bounded frame cache sized from available memory. The UI hands
it commands that never block.

**Audio.** A new `spherecut-audio` crate plays through cpal's PulseAudio
protocol host, with ALSA as the fallback. The audio clock drives A/V sync.

**GUI.** A new `spherecut-app` crate holds the eframe GUI:

- docked panes;
- the viewer as a wgpu paint callback;
- grab-the-sphere, zoom and the projection switch;
- transport controls;
- the performance overlay;
- the startup doctor banner.

**Hardware.** hw-test gains a GUI smoke run, a realistic 5.7K perf clip
and Tier 1 runs on the hardware machine.

### 1.3 Corrections to facts the designs stated

- huginn's Fedora 44 RPM is rustc **1.99.0** (PR #2), not 1.95 as D1 says.
  The workspace MSRV still moves to 1.95.
- `AVFrame.duration` arrived in libavutil 57.43, so it exists in every
  FFmpeg from 6.0 on (6.1.1 here, 8.1.3 on huginn).
- WGSL allows `atan2` and `asin` errors of thousands of ULP. GPU-against-CPU
  error therefore grows with image radius (lodprobe: 40.8 dB at the tiny
  planet's rim, 55.8 dB for fisheye 360°), and tolerances are calibrated
  per view, never assumed from f32 precision.
- `cargo --all-features` turns on only workspace members' features.
  cpal 0.18.2 has `default = []`, so its `pipewire` feature is reached only
  if one of our crates forwards it. We don't use it because it needs
  libpipewire-0.3 dev and libclang at build time, and this box has no
  libpipewire dev. If we ever added an opt-in forwarding feature, `cargo
  xtask check`'s `clippy --all-features` would build it and fail here.
- egui-wgpu 0.37's default surface configuration is already AutoVsync with
  frame latency 2. The app leaves it alone.
- `Queue::write_texture` in wgpu-core 30 creates a transient staging buffer
  per call. wgpu-hal suballocates it from gpu-allocator blocks (64 to 128
  MB host-visible under `MemoryHints::Performance`), so a 5.7K frame
  does not cost a `vkAllocateMemory`. A 10-bit 8K pair stages about 88 MB
  per frame, which can overflow the current block, and gpu-allocator frees
  an empty block unless it is the last one. Blocks may then be allocated
  and freed every frame. `bench_upload_5k7` measures this before anything
  is changed (section 4.4).

## 2. Crate layout

### 2.1 Crates and dependency direction

```
spherecut-core      time, colour and frame vocabulary, cancel, clocks, may_block, TempFile
spherecut-sphere    projection math, Euler convention, grab, pixel mapping       -> core
spherecut-media     FFmpeg: confinement, probe, doctor, decode, seek, audio      -> core
spherecut-render    wgpu: planes, upload, mips, projection, encode, still, reference -> core, sphere
spherecut-audio     cpal output, sample ring, audio clock, null and fake devices -> core
spherecut-playback  players, workers, frame cache, GPU slots, presenter, stats   -> core, sphere, media, render, audio
spherecut-app       eframe GUI: tiles, viewer callback, transport, overlay        -> playback, render, media, sphere, core
spherecut-cli       the `spherecut` binary: probe, doctor, frame, the GUI by default -> app, media, render, sphere, core
spherecut-synth     generated media, fake backends, perf clip generator          -> core, sphere, media
spherecut-testkit   tier2 gates, bench recording, PSNR/SSIM/diff_stats, FNV hash  (no deps)
xtask               check, gen-media, hw-test
```

Rules:

- `render` never depends on `media`. Frames reach it through
  `core::video::VideoPlanes`, which both `media::VideoFrame` and synth's
  in-memory frames implement. Goldens therefore run without FFmpeg, and
  M3's stitcher can feed `render` from any source.
- `playback` never depends on egui. M2's export, `spherecut render` and the
  Tier 2 real-time bench drive the same workers with a frame-stepped or
  real-time clock and no window.
- `synth` depends on `media`, so tests that need both (the seek oracle,
  fallback tests) live in `crates/synth/tests/`, following the existing
  `decode_errors.rs` pattern. `media`'s dev-dependencies stay free of
  `synth`.
- The `spherecut` binary links eframe, but winit loads libwayland-client
  and libX11 with dlopen only when the GUI starts. `spherecut probe` on a
  headless server never touches them.

### 2.2 What is new or changed, per crate

| Crate | New or changed |
|---|---|
| core | `video.rs`: `PixelLayout`, `PlaneDesc`, `PlaneData`, the `VideoPlanes` trait, `ColorInfo` (codes, range, matrix as Kr/Kb, transfer, primaries, siting, `assumed` flags). `cancel.rs`: `StopSource`, `StopToken`. `clock.rs`: the `Clock` trait, `ManualClock`, `SystemClock`. `blocking.rs`: `mark_ui_thread`, `may_block`. `time.rs`: `TimeSpec` and its grammar, serde for `Time` as `{exact, seconds}`, `Rational` as `"n/d"`, and `FrameTime` (a `Time` plus an optional frame index). `fs.rs`: `TempFile` moved from `media::inject`, with `persist_replace` (D16) and the new `persist_new` (no clobber) |
| sphere | `pixel.rs` (pixel centre to lens point and back), `orient.rs` (yaw, pitch and roll), `grab.rs` (level and free grab, zoom about a point), `equirect::uv_gradient`, `Lens::project_morph` |
| media | `ffi` becomes a directory module (`ffi/mod.rs` from today's `ffi.rs`, plus `confine.rs`, `pool.rs`, `resample.rs`, `scale.rs`). New: `backend.rs`, `seek.rs`, `hw.rs` (render node lookup), `color.rs` (default rules), `audio.rs`. `probe` and `doctor` open through the confined input. `MediaError::code()` |
| render | new crate (section 4) |
| audio | new crate (section 9) |
| playback | new crate (section 8) |
| app | new crate (section 7), with its own `clippy.toml` |
| cli | `frame.rs`, `contract.rs`, `interrupt.rs` (the one FFI module), `Cli.cmd` becomes `Option<Cmd>` (in slice 7) |
| synth | `fake.rs`: `SyntheticBackend`, `GatedBackend`, `FaultBackend`. New clips (section 12.1). The noisy 5.7K perf clip generator. `ColorTags` replace `bt709_limited` in `encode::VideoSettings` |
| testkit | `compare.rs` (`psnr`, `ssim`, `diff_stats`), `fnv64`, `tier2::require_graphical_session`, `tier2::require_audio_device` |
| xtask | `check`: strace and ALSA prerequisites. `hw-test`: the additions in section 12.4 |

### 2.3 rust-version

The workspace `rust-version` moves from 1.88 to **1.95** in slice 3, when
wgpu lands (egui 0.37 needs 1.95). As D1 did for 1.88, the slice checks it
with `cargo +1.95 build --workspace --all-targets` and `cargo +1.95 test
--workspace`, after `rustup toolchain install 1.95` (a build-time
download, not a runtime one). This box runs 1.97 stable, and huginn runs
1.99 from Fedora's RPM.

### 2.4 unsafe policy

- `#![forbid(unsafe_code)]` goes in core, sphere, render, audio, playback,
  app, synth, testkit and xtask. bytemuck's `derive(Pod, Zeroable)`
  compiles under `forbid` (checked in a scratch crate,
  `scratchpad/m1/plancheck/bm`), so render needs no unsafe in M1.
- In media, `#![deny(unsafe_code)]` goes at the crate root, with
  `#[allow(unsafe_code)]` on `mod ffi` and on `mod hwprobe`, the two FFI
  modules D11 allows. Today these are the only files with `unsafe`
  (checked with grep).
- In cli, `#![deny(unsafe_code)]` goes at the root, with
  `#[allow(unsafe_code)]` on `mod interrupt`, which wraps `sigaction`.
- Every block keeps a SAFETY comment. The workspace already denies
  `clippy::undocumented_unsafe_blocks`.
- M9's DMA-BUF import is the first GPU-side unsafe, in an FFI module of
  its own.

## 3. MediaBackend: signatures and seek semantics

### 3.1 Shared vocabulary (`spherecut-core::video`, `cancel`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleBytes { One, Two }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneDesc {
    pub components: u8,          // 1 (Y, U, V, G, B, R) or 2 (interleaved UV)
    pub bytes: SampleBytes,
    pub log2_sub: (u8, u8),      // horizontal and vertical subsampling against luma
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelLayout {
    pub name: String,            // FFmpeg's name, for display only
    pub planes: Vec<PlaneDesc>,  // 1..=3; alpha is never included
    pub family: ColorFamily,     // Yuv | Rgb (gbrp: G, B, R planes) | Gray
    pub bit_depth: u8,           // 8..=16
    pub msb_aligned: bool,       // P010/P016: samples in the high bits
}

#[derive(Debug, Clone, Copy)]
pub struct PlaneData<'a> { pub bytes: &'a [u8], pub stride: usize, pub width: u32, pub height: u32 }

/// A decoded picture whose planes can be uploaded without copying them first.
pub trait VideoPlanes: Send + Sync + std::fmt::Debug {
    fn layout(&self) -> &PixelLayout;
    fn size(&self) -> (u32, u32);
    fn plane(&self, index: usize) -> Option<PlaneData<'_>>;
    fn color(&self) -> &ColorInfo;
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ColorInfo {
    pub codes: ColorCodes,       // H.273 ints as coded: primaries, transfer, matrix, range, chroma_location
    pub range: Range,            // Limited | Full
    pub matrix: Matrix,          // Bt709 | Bt601 | Bt2020Ncl | Smpte240m | Fcc | Identity; kr_kb() -> (f64, f64)
    pub transfer: Transfer,      // Sdr | Pq | Hlg | Linear
    pub primaries: Primaries,    // Bt709 | Bt2020 | Other(u8)
    pub siting: ChromaSiting,    // Left | Center | TopLeft
    pub assumed: Assumed,        // one bool per field: true when a default rule chose it
}

// crates/core/src/cancel.rs
#[derive(Debug, Clone)] pub struct StopSource { gen: Arc<AtomicU64>, shutdown: Arc<AtomicBool> }
#[derive(Debug, Clone)] pub struct StopToken  { gen: Arc<AtomicU64>, mine: u64, shutdown: Arc<AtomicBool> }
impl StopSource { pub fn new() -> Self; pub fn next(&self) -> StopToken; pub fn shutdown(&self); }
impl StopToken  { pub fn is_stopped(&self) -> bool; /* gen != mine (Acquire) || shutdown */ }
```

A newer seek bumps the generation, so an older one stops at its next
packet or frame boundary. FFmpeg's interrupt callback (the existing AVIO
cancel flag) fires only at shutdown and for an abandoned open, because D13
makes an interrupted read terminal. A superseded seek therefore never
leaves the demuxer in an unknown state.

### 3.2 The traits (`crates/media/src/backend.rs`)

```rust
pub trait MediaBackend: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    /// Blocking. The confined open (section 3.6) plus our MP4/Matroska checks.
    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError>;
    /// Blocking. A confined demuxer and decoder for one video stream. Decodes the
    /// first presented frame to fix the clip origin.
    fn open_video(&self, path: &Path, req: &VideoRequest) -> Result<Box<dyn VideoDecode>, MediaError>;
    /// Blocking. Its own confined demuxer (VideoDecoder discards the other streams, D13).
    fn open_audio(&self, path: &Path, req: &AudioRequest) -> Result<Box<dyn AudioDecode>, MediaError>;
}

#[derive(Debug, Clone)]
pub struct VideoRequest {
    pub stream: Option<usize>,
    pub decode: DecodeMode,          // Software | Hardware { kind: HwKind, device: Arc<HwDevice>, fallback: Fallback }
    pub threads: Option<NonZeroU32>, // software only; None = FFmpeg's auto (D3)
    pub extra_hw_frames: u8,         // 2 in M1: one held best frame during a seek, one spare
    pub interrupt: Arc<AtomicBool>,  // shutdown only
    #[cfg(feature = "fault-injection")]
    pub fault: Option<FaultPlan>,    // tests: fail frame k "as hardware"
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum HwKind { VaApi, Vulkan }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Fallback { Software, Fail }

pub trait VideoDecode: Send + std::fmt::Debug {
    fn info(&self) -> &VideoStreamInfo;
    fn path(&self) -> &DecodePath;   // Software{threads} | Hardware{kind} | FellBack{from, reason}
    /// Clip time: 0 is the first presented frame.
    fn seek(&mut self, at: Time, mode: SeekMode, stop: &StopToken) -> Result<SeekOutcome, MediaError>;
    /// The frame after the last one returned by seek or next_frame.
    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError>;
    /// Some(t): skip non-reference frames (AVDISCARD_NONREF) and never download or return
    /// a frame before t. None: back to normal. Used when playback falls behind the clock.
    fn catch_up(&mut self, until: Option<Time>);
    fn take_events(&mut self) -> Vec<DecoderEvent>; // FellBack, LayoutChanged, Converted, Damaged
    fn stats(&self) -> DecodeStats;                 // per-frame decode and download ms, frames decoded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum SeekMode { Exact, Keyframe }

#[derive(Debug)]
pub enum SeekOutcome {
    Frame { frame: Arc<VideoFrame>, exactness: Exactness, cost: SeekCost },
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exactness {
    Exact,
    BeforeFirst,                     // at < 0: the first frame
    AfterLast,                       // at >= duration: the last frame
    KeyframeOnly,                    // SeekMode::Keyframe
    Truncated { last_good_pts: i64 },// the file is damaged before `at` (D13)
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekCost { pub seeks: u32, pub reseeks: u32, pub frames_decoded: u32, pub forward_only: bool }

#[derive(Debug)] pub enum NextFrame { Frame(Arc<VideoFrame>), End, Stopped }

#[derive(Debug, Clone)]
pub struct VideoStreamInfo {
    pub index: usize,
    pub codec: String,
    pub size: (u32, u32),            // after cropping
    pub time_base: Rational,
    pub origin_pts: i64,             // pts of the first presented frame
    pub duration: Option<Time>,      // clip time at which the last frame's interval ends
    pub timing: Timing,              // Constant(FrameRate), exact from our stts reader | Variable { average }
    pub frame_count: Option<u64>,
    pub layout: PixelLayout,         // what frames carry: native, or the swscale target
    pub source_pix_fmt: String,
    pub color: ColorInfo,
    pub mapping: SourceMapping,      // equirect uv_rect from sv3d bounds, stereo eye, pose_present
    pub rotation_deg: Option<f64>,
}
```

`VideoFrame` is owned, immutable and `Send + Sync`. It is shared as
`Arc<VideoFrame>` between the cache and the upload thread, so no plane is
copied to share it:

```rust
#[derive(Debug)]
pub struct VideoFrame {
    pub pts: i64,                   // AVFrame.pts, else best_effort_timestamp (read as ints, D11)
    pub time: Time,                 // (pts - origin_pts) * tb, exact
    pub duration: Option<Time>,     // to the next pts when known, else AVFrame.duration
    pub key: bool,
    pub concealed: bool,            // decode_error_flags != 0
    pub decoded_on: DecodePath,
    pub run: u64, pub seq: u64,     // seek generation and decode order: the cache links runs by these
    storage: FrameStorage,          // Ffmpeg(ff::frame::Video) | Owned { planes, strides }
    layout: PixelLayout, color: ColorInfo, bytes: usize,
}
impl VideoPlanes for VideoFrame { /* zero copy through ffi::plane's bounded slices */ }
```

Static assertions (`const _: () = { fn send<T: Send>() {} ... }`) on
`VideoFrame`, `VideoDecoder` and `Box<dyn VideoDecode>` keep an
ffmpeg-next change from silently dropping `Send` or `Sync`.

Audio, in the same module:

```rust
#[derive(Debug, Clone)]
pub struct AudioRequest { pub stream: Option<usize>, pub out: AudioOutFormat, pub interrupt: Arc<AtomicBool> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct AudioOutFormat { pub rate: u32, pub channels: u16 }

pub trait AudioDecode: Send + std::fmt::Debug {
    fn info(&self) -> &AudioStreamInfo;      // codec, rate, channels, layout, ambisonic (SA3D, D17), start
    /// The next chunk starts exactly at output sample round(at * rate) of the clip timeline.
    fn seek(&mut self, at: Time, stop: &StopToken) -> Result<(), MediaError>;
    fn next_chunk(&mut self, stop: &StopToken) -> Result<NextAudio, MediaError>;
}
#[derive(Debug)] pub struct AudioChunk { pub start: i64 /* output-rate sample index, clip time */, pub channels: u16, pub data: Vec<f32> }
#[derive(Debug)] pub enum NextAudio { Chunk(AudioChunk), End, Stopped }
```

New `MediaError` variants, each with a stable `code()`:

| Variant | code | Exit (CLI) |
|---|---|---|
| `UnsupportedContainer { path, format }` | `unsupported_container` | 1 |
| `Refused { path, url }` (a nested open was attempted) | `nested_open_refused` | 1 |
| `TooLarge { path, width, height, max }` | `too_large` | 1 |
| `NoDecoder { path, codec }` | `no_decoder` | 3 for `frame`; a warning for probe; doctor reports its own checks (exit 1) |
| `UnsupportedFormat { path, pix_fmt }` | `unsupported_pixel_format` | 1 |
| `NotFound` from `Io` (including a URL-shaped path) | `not_found` | 1 |
| existing `HwUnsupported`, `Hardware` | `hw_unsupported`, `hw_unavailable` | 3 when the user asked for `--decoder vaapi` |

`Cancelled` is not a `MediaError`: a stopped seek returns
`SeekOutcome::Stopped`.

### 3.3 Seek semantics (the contract)

Definitions:

- **Presented frames.** P is the sequence of frames a sequential decode
  from the start outputs. FFmpeg's mov demuxer has already applied the
  edit list: packets flagged `AV_PKT_FLAG_DISCARD` and their frames are
  dropped. The order is presentation order.
- **pts.** pts(f) is `AVFrame.pts`, falling back to
  `best_effort_timestamp`.
- **Interval.** Frame fᵢ is shown on [pts(fᵢ), pts(fᵢ₊₁)). The last frame
  is shown on [pts, pts + d), where d is `AVFrame.duration`, or the average
  frame duration when that is 0.
- **Clip time.** τ = (pts − origin_pts) × tb. origin_pts is pts(f₀), found
  by decoding at open. **τ = 0 is the first presented frame**, whatever the
  edit list or B-frame delay. The container's `start_time` is not used.
  `--time` and the GUI's clock are clip times. Audio is placed on the same
  axis by its own pts.

**`seek(at, Exact)`** returns the frame with the greatest pts ≤ T, where
T = origin_pts + ⌊at / tb⌋. That is computed with `Time::to_pts(tb,
Rounding::Floor)`, so the comparison is exact integer arithmetic. Edge
rules:

- **Ties** (equal pts in a broken VFR mux): the frame later in output order
  wins, so a zero-length interval is never shown.
- **at < 0:** f₀, flagged `BeforeFirst`.
- **at ≥ duration:** the last frame, flagged `AfterLast`. The GUI shows it.
  `spherecut frame` refuses it (section 5).
- **Damaged before T:** the last good frame, flagged `Truncated`, plus a
  `Damaged` event (D13).
- **pts going backwards** in a sequential decode marks the stream "not
  exactly seekable". Exact seeks then decode from the start: correct, slow,
  and announced with a notice.
- The next `next_frame()` returns the frame after the one returned.

**The algorithm** (`crates/media/src/seek.rs`), as seekprobe measured it
(0 misses on open-GOP HEVC and H.264, edit lists and true VFR, against 11/40
and 8/40 for a naive BACKWARD seek):

```
seek_exact(T):
  if forward_is_cheaper(T): decode forward from the current position (no av_seek)
  else:
    ts = T; prev_d0 = None
    for attempt in 0..8:
      av_seek_frame(stream, ts, AVSEEK_FLAG_BACKWARD); flush the decoder; drop lookahead
      d0 = dts of the first packet of our stream after the seek
      f  = first frame out, skipping AV_FRAME_FLAG_CORRUPT frames
      if pts(f) <= T or pts(f) == origin_pts: break             // at or before the target
      if d0 is NOPTS or (prev_d0 is Some and d0 >= prev_d0): goto FROM_START   // no progress
      prev_d0 = Some(d0); ts = d0 - 1                            // strictly before that keyframe
    if no break: FROM_START: seek to byte 0 and flush
  decode forward; keep the best frame with pts <= T; the first frame with pts > T stays as lookahead
  return the best frame (f0 when T < origin_pts)
```

- **Why the re-seek.** A BACKWARD seek can land on a keyframe whose dts is
  at or before T but whose pts is after T. Its leading pictures (HEVC RASL
  after a CRA, or open-GOP B-frames) are then dropped silently, with no
  CORRUPT flag. The re-seek to d0 − 1 lands strictly before that keyframe.
- **`forward_is_cheaper(T)`** holds when T is ahead of the current position
  and the keyframe that `av_index_search_timestamp(st, T, BACKWARD)` names
  is at or before the next packet to be read, so T lies in the GOP being
  decoded. Past the next keyframe, a seek is never more decode work than
  going forward. Without an index, T − current ≤ 1 s decodes forward.
  Frame step forward and short scrubs within a GOP therefore never seek.
- **Nothing before T is downloaded.** With VA-API, the best candidate so
  far is held as a hardware frame, and only the final one is downloaded.
  `extra_hw_frames = 2` keeps the surface pool from running dry while one
  is held. This saves about 3 ms per skipped 5.7K frame.
- **State reset.** A seek clears truncation and read failures, so a seek
  back into the good part of a truncated file works again. It does not
  clear `refused` (the hardware said no). This updates D13, where a read
  error was sticky across seeks.
- **`SeekMode::Keyframe`** (scrubbing while the pointer moves): one
  BACKWARD seek, then the first non-corrupt frame whatever its pts, flagged
  `KeyframeOnly`. The decoder stays positioned, so the settle step runs
  forward without a second seek.
- **Step back:** `seek(Time::from_pts(cur.pts − 1 − origin_pts, tb),
  Exact)`. That is the greatest pts below the current one, which is exact
  for CFR and VFR alike. The cache answers it instantly when the
  predecessor is held.
- **Frame-index addressing** (`--frame N`, the GUI counter):
  - CFR, detected exactly from `stts` by our MP4 reader (or from Matroska
    block timing): the target is (N + ½) × frame_duration. That time lies
    strictly inside frame N's interval even when the container time base
    rounds pts. In Matroska at 1/1000 the error is at most 0.5 ms, against
    a half interval of at least 8 ms at 60 fps. An MKV-remux test proves
    it.
  - VFR: frames are counted from the start. That is exact and slow, so it
    comes with a notice.
  - The displayed frame number for CFR is round(τ × fps).
- **Raw streams with no timestamps:** pts are synthesized as index ×
  frame_duration at open, and exact seeks always decode forward.

### 3.4 Pixel formats

- **Native, uploaded as they are.** These need no CPU conversion:
  - semi-planar `nv12`, `nv16`, `p010le`, `p210le`, `p016le`;
  - planar `yuv{420,422,444}p` (and `yuvj*`) at 8, 10, 12 and 16 bits;
  - `gray`, `gray10le`, `gray12le`;
  - `gbrp`, `gbrp10le`, `gbrp12le`.

  Alpha planes are ignored. `ffi::pixel_layout(format)` builds the
  `PixelLayout` from `AVPixFmtDescriptor`, read as ints (D11). 4:2:2,
  4:4:4 and 12-bit ProRes or HEVC therefore reach the GPU at full chroma
  resolution and full depth.
- **Everything else** goes through swscale on the decode worker:
  packed `yuyv422`, big-endian, `rgb24`, `pal8`, `nv21` and so on. The
  target is `yuv444p` for 8 bits and `yuv444p16le` above, so no chroma is
  lost. The worker raises `DecoderEvent::Converted`, the overlay shows the
  cost, and a format swscale refuses gives `UnsupportedFormat`.
- **VA-API downloads** come as `nv12` or `p010le`, through the pooled
  download (section 3.5).

### 3.5 Colour defaults, VA-API, the render node and fallback

**Colour defaults** (`media::color::resolve`). Each defaulted field sets
its `assumed` flag. The Info pane, the `frame` JSON and the probe JSON
show the flags.

| Field | Rule when unspecified or reserved |
|---|---|
| range | `AVFrame.color_range` read as an int, which `av_frame_copy_props` keeps across a VA-API download. A `yuvj*` format means Full. Otherwise Limited |
| matrix | BT.709 when width ≥ 1280 or height > 576, else BT.601. This is mpv's and libplacebo's rule, not FFmpeg's: swscale treats unspecified as BT.601 and libavcodec does not guess. Identity for `gbrp*` |
| transfer | `Sdr`. PQ and HLG render untransformed, with the notice "HDR shown without tone mapping (M7)" |
| primaries | BT.709. BT.2020 renders without gamut mapping, with a notice (M7) |
| siting | Left for 4:2:0 and 4:2:2, the H.264, HEVC, AV1 and ProRes convention. Center for MJPEG. Siting is decided by codec, not by the `yuvj` range flag. Synth's clips are 2×2 averages, so they are tagged Center; slice 2 checks that libx264 and libx265 write `chroma_loc` to the VUI, and otherwise the tests pass a `ColorInfo` override |

**Pooled download.** Today `ffi::download_frame` lets
`av_hwframe_transfer_data` allocate a fresh 25 MB (5.7K NV12) or 50 MB
(P010) buffer per frame. `ffi::download_into_pool` replaces it:

- buffers come from an `AVBufferPool` sized from
  `hw_frames_ctx->sw_format`;
- each plane's linesize is rounded up to 256 bytes, so the upload's
  `write_texture` takes wgpu-core's single-copy path (stage pitch equals
  source pitch);
- buffers return to the pool when the cache drops the frame;
- the pool is rebuilt on `LayoutChanged`.

**The render node** of the GPU wgpu renders on, in `media::hw` over a
sysfs root that tests can fake:

1. Read `wgpu::AdapterInfo.device_pci_bus_id` (wgpu 30; `"{:04x}:{:02x}:{:02x}.{}"`,
   or empty when unknown) and look up
   `/sys/bus/pci/devices/<id>/drm/renderD*`.
2. Otherwise scan `/sys/class/drm/renderD*/device/{vendor,device}` for the
   adapter's ids.
3. Otherwise use `/dev/dri/renderD128`.

This matters on odin if its CPU has an enabled iGPU. Decoding on one GPU
and rendering on the other would add a PCIe copy and rule out M9's
zero-copy. The doctor, the GUI's startup check and `frame` all use the
same lookup. `--render-node` overrides it.

One VA-API device is opened per backend (a `OnceLock` holding a
`Result<Arc<HwDevice>, String>`) and shared across decoders. Hardware
decoders stay single-threaded (`ffi::hw_choice`).

**Fallback** lives in the backend wrapper (D3, D14). With `Fallback::Software`:

- **At open:** `HwUnsupported` or a device that won't open makes the
  wrapper open software instead. It queues `FellBack { reason }`.
- **Mid-stream:** a hardware decode or transfer error on frame k makes the
  wrapper reopen in software and run `seek_exact` to the pts after the
  last frame it returned. It queues `FellBack { at }`. The caller sees the
  same frame sequence, with no gap and no repeat.
- **With `Fallback::Fail`**, used by every Tier 2 test, the error is
  returned.
- **Surfaced four ways:** a `tracing::warn!` with D14's reason; the
  overlay's decoder line and a "Software decode" badge; a dismissible GUI
  notice with the doctor's fix when one matches; and `note: decoding in
  software: <reason>` on stderr from `frame`, plus a `warnings` entry in
  its JSON.

Tier 1 covers both paths:

- FFmpeg's Vulkan hwdevice on lavapipe has no video decode, so opening
  with it must fall back with `Refused(NotOffered)`. That is a real path,
  not a mock (the `hw_refusal.rs` pattern).
- The `fault-injection` feature (enabled only by synth's dev-dependency)
  fails frame k "as hardware". The output must equal sequential software
  decode bit for bit, with exactly one event.

### 3.6 FFmpeg opens only the file Spherecut opened

`ffi::confine::open_input(path, interrupt) -> Result<ConfinedInput,
MediaError>` replaces M0's open. probe, `VideoDecoder`, the audio decoder
and doctor's file checks all use it.

1. **The file.** `std::fs::File::open(path)` runs in Rust, which takes any
   byte path, so D11's non-UTF-8 handling comes for free.
   `http://127.0.0.1:1/x.mp4` is an ordinary relative path that does not
   exist. It fails with `not_found`, and the message adds "Spherecut opens
   local files only" when the path looks like a URL. No socket is created.
2. **Our own AVIO.** `ff::format::context::StreamIo::from_read_seek(file)`
   (ffmpeg-next 9.0.0; the repo already uses `from_write_seek` in
   `encode.rs`). FFmpeg reads bytes only through it.
3. **Our own open**, modelled on ffmpeg-next's `input_from_stream_impl`:
   - `avformat_alloc_context`, then set `pb`, `AVFMT_FLAG_CUSTOM_IO` and
     the interrupt callback;
   - set **`io_open = refuse_io_open`**: an `extern "C"` function that
     records the URL through `AVFormatContext.opaque` into a log we own
     (`Arc<Mutex<Vec<String>>>`), guarded by `catch_unwind`, and returns
     `AVERROR(EPERM)`.
4. **Our own container check.** `av_probe_input_buffer2(pb, &fmt, ...)`,
   then compare `fmt.name` with `ALLOWED_DEMUXERS =
   ["mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm"]`. Anything else is
   `UnsupportedContainer { format }` ("hls", "concat", "lrc", ...). The
   chosen `fmt` goes to `avformat_open_input`, so FFmpeg does not probe
   again. Widening the list needs a DECISIONS entry.
5. **Options set anyway**, as defence in depth and the letter of the
   criterion: `protocol_whitelist=file` and
   `format_whitelist=mov,mp4,m4a,3gp,3g2,mj2,matroska,webm`.
6. **A refused nested open.** If open or `find_stream_info` fails while
   the refusal log is non-empty, the error is `Refused { url }`.
7. **`max_pixels`.** `ffi::open_video_decoder` and probe's `open_decoder`
   set `av_opt_set_int(ctx, "max_pixels", 16384 * 8192, 0)` before
   `avcodec_open2`. That is a 16K equirect, 134 MP; 8K is 29.5 MP.

`ConfinedInput { input, refused }` drops `input` first, by field order.

**Clippy.** The root `clippy.toml` gains `disallowed-types` for
`std::net::TcpStream`, `std::net::TcpListener` and `std::net::UdpSocket`,
each with the reason "no network access at runtime (D8, D23)". Unix
sockets, which A3's attach socket uses, stay allowed.

**probe refuses other containers by name**, with exit 1, instead of
reporting them with `structure: none`. Several things change with it:

- the `lrc` test (`crates/cli/tests/probe.rs:726`,
  `files_without_video_or_audio_fail`) expects the refusal;
- the README's probe section ("other containers not at all, which a
  `structure` line says", and `"none"` in the JSON `structure.check`);
- D15's "other containers not at all, and the report says so" sentence,
  which becomes "other containers are refused by name (D24)".

## 4. Renderer and shader (`spherecut-render`)

### 4.1 wgpu setup

```rust
#[derive(Debug, Clone)]
pub enum AdapterChoice { Auto, Cpu, Named(String) }  // Auto: discrete, integrated, virtual, then CPU (lavapipe)

#[derive(Debug, Clone)]
pub struct GpuCaps {
    pub info: wgpu::AdapterInfo,
    pub is_cpu: bool,
    pub timestamps: bool,              // TIMESTAMP_QUERY
    pub timestamps_in_encoders: bool,  // TIMESTAMP_QUERY_INSIDE_ENCODERS
    pub anisotropy: bool,              // DownlevelFlags::ANISOTROPIC_FILTERING
    pub max_texture_2d: u32,
    pub timestamp_period_ns: f32,
}

#[derive(Debug, Clone)]
pub struct Gpu { pub instance: wgpu::Instance, pub adapter: wgpu::Adapter,
                 pub device: wgpu::Device, pub queue: wgpu::Queue, pub caps: GpuCaps }
impl Gpu {
    pub fn headless(choice: &AdapterChoice) -> Result<Gpu, RenderError>;                // frame, tests, benches
    pub fn from_parts(instance: wgpu::Instance, adapter: wgpu::Adapter,
                      device: wgpu::Device, queue: wgpu::Queue) -> Gpu;                 // eframe's device
    pub fn instance_descriptor() -> wgpu::InstanceDescriptor;                          // Backends::VULKAN only
    pub fn select_adapter(adapters: &[wgpu::Adapter], choice: &AdapterChoice) -> Result<wgpu::Adapter, RenderError>;
    pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static>;
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    NoAdapter { tried: Vec<String> },          // code no_gpu, exit 3, with the doctor's Vulkan fix
    AdapterNotFound { wanted: String, available: Vec<String> }, // code adapter_not_found, exit 3
    Device(String),
    TooLarge { w: u32, h: u32, max: u32 },     // code too_large, exit 1
    Readback(String),
    OutOfMemory(&'static str),
}
```

Details:

- **Instance.** The Vulkan backend only. eframe gets the same descriptor
  through `WgpuSetupCreateNew::instance_descriptor`.
- **Adapter.** `SPHERECUT_ADAPTER` is honoured (egui-wgpu ignores
  `WGPU_ADAPTER_NAME`). Tests and goldens pass `AdapterChoice::Cpu` and
  assert `DeviceType::Cpu`, so nothing changes the environment
  in-process (edition 2024 makes `set_var` unsafe). Auto falls back to
  lavapipe, so cloud and agent boxes work. The GUI shows a warning when it
  lands on a CPU adapter.
- **Device.** Required features are `adapter.features() ∩
  (TIMESTAMP_QUERY | TIMESTAMP_QUERY_INSIDE_ENCODERS)`. Limits are
  `Limits::default()` with `max_texture_dimension_2d` raised to the
  adapter's own limit, capped at 16384 (RADV and lavapipe report 16384,
  and 8192 is not enough for 11K sources). Memory hints are
  `Performance`.
- **No optional texture features.** Every format below is guaranteed by
  wgpu-types 30 without `TEXTURE_FORMAT_16BIT_NORM` or
  `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`.

### 4.2 Plane textures, mips and GPU slots

| Source plane | Upload texture | Canonical texture (sampled, mipped) |
|---|---|---|
| 8-bit, one component | `R8Unorm` (the canonical one, written directly) | same |
| 8-bit, interleaved UV | `Rg8Unorm` (direct) | same |
| 9 to 16 bits, one component | `R16Uint` scratch, one per uploader | `R16Float` holding code / (2ᵇ − 1) |
| 9 to 16 bits, interleaved UV | `Rg16Uint` scratch | `Rg16Float` |

`R8Unorm`, `Rg8Unorm`, `R16Float` and `Rg16Float` are guaranteed
renderable and filterable. `R16Uint` and `Rg16Uint` are guaranteed as
copy destinations read with `textureLoad`.

- **The ingest pass** is one render pass per deep plane. It reads the
  scratch with `textureLoad` and writes `f32(code >> shift) / (2ᵇ − 1)`
  into the canonical level 0. The shift is 6 for P010 and 0 for
  LSB-aligned formats.
- **Precision.** f16 keeps 11 significant bits, so the worst rounding error
  near 1.0 is 2.4e-4: a quarter of a code at 10 bits and one code at 12
  bits. 16-bit sources lose their low 4 bits on screen, which M7 can
  revisit with an `R16Unorm` path behind the optional features; that path
  would reuse the same mip shader.
- **Cost.** At 10-bit 5.7K the pass reads and writes about 50 MB per frame,
  roughly 0.4 ms at 256 GB/s. `bench_upload_5k7` measures it.

**Mips.** One area-weighted mip shader serves all four canonical formats.
It runs as a render pass per level per plane:

- It reads level n − 1 with `textureLoad` and weights each source texel by
  the area it shares with the destination texel. That takes at most 3×3
  taps, and odd (NPOT) sizes come out exactly.
- wgpu tracks usage per subresource, so a level can be sampled while the
  next one is written.
- The chain is capped near 64 px: `levels = clamp(1 + floor(log2(max(w,
  h) / 64)), 1, 8)`. A 5760×2880 luma plane gets 7 levels and its
  2880×1440 chroma 6, both ending at 90×45.
- NV12 at 5.7K takes 11 passes per frame and three-plane 4:2:0 takes 16,
  against 23 and 34 for full chains.
- A destination texel never straddles the ±180° column, so the box filter
  needs no seam handling.
- Mips are generated for every uploaded frame.

**Slots.** A slot is one uploaded frame. It holds:

- the canonical textures with their mip chains;
- a bind group;
- a `SourceUniform` buffer;
- the frame's pts, run, layout and size.

A 5.7K 8-bit NV12 slot is about 33 MB with mips, and a 10-bit one about
66 MB. The default is **4 slots**: one shown, one released and awaiting
its last draw, and two ready ahead. That is 133 MB at 8 bits and 265 MB
at 10 bits, plus the scratch. The slot count is a setting from 3 to 6.
Slots are allocated inside `push_error_scope(OutOfMemory)`, and a failure
drops one slot with a notice.

### 4.3 The projection shader (`shaders/project.wgsl`)

There is one fragment pass over a full-screen triangle, into `Rgba16Float`.
The uniforms are `#[repr(C)]` + bytemuck structs, and each `mat3` is
passed as three `vec4`. A dev-only naga test (section 4.5) checks the
offsets.

```wgsl
override FILTER: u32 = 0u;     // 0 isotropic (textureSampleLevel with our λ), 1 anisotropic (textureSampleGrad, 16×)
override DEBUG_MODE: u32 = 0u; // 0 normal, 1 implicit textureSample, 2 level 0 only, 3 LOD heat map (1 and 2 are negative controls)

struct View {
  rot0: vec4<f32>, rot1: vec4<f32>, rot2: vec4<f32>, // view to world, columns (q is f64 on the CPU, f32 here)
  size: vec2<f32>,                                     // target width and height in pixels
  half: vec2<f32>,                                     // half hfov of `from` and `to`
  proj: vec2<u32>,                                     // 0 rectilinear, 1 fisheye (equidistant), 2 stereographic
  morph: f32, _pad: f32,
};
struct Source {
  m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,  // R'G'B' = dot(m_i, (s0, s1, s2, 1)): code scale, range and Kr/Kb folded in
  chroma_offset: vec2<f32>,                      // siting: Left = (0.5 / W_luma, 0), TopLeft adds 0.5 / H_luma
  layout: u32,                                   // 0 interleaved UV, 1 planar, 2 gray, 3 GBR
  transfer: u32,                                 // 0 sRGB EOTF (SDR), 1 pass-through
  uv_scale: vec2<f32>, uv_offset: vec2<f32>,     // sphere uv to texture uv: sv3d crop, then the stereo eye
  cover: vec4<f32>,                              // the sphere-uv rectangle the frame covers; outside it is black
  size0: vec2<f32>, size1: vec2<f32>,            // level-0 sizes of the luma and chroma textures
  levels: vec2<f32>,                             // mip counts, to clamp λ
};

struct Ray { d: vec3<f32>, theta: f32 };
fn ray_at(px: vec2<f32>) -> Ray {                // sphere::pixel::to_lens, Lens::unproject with morph, then q
  let p = vec2(2.0 * px.x / view.size.x - 1.0, (1.0 - 2.0 * px.y / view.size.y) * view.size.y / view.size.x);
  let rho = length(p);
  let theta = mix(theta_of(view.proj.x, rho, view.half.x), theta_of(view.proj.y, rho, view.half.y), view.morph);
  let a = select(vec2(0.0), p / rho, rho > 1e-12);
  let d = normalize(mat3x3(view.rot0.xyz, view.rot1.xyz, view.rot2.xyz) * vec3(sin(theta) * a, -cos(theta)));
  return Ray(d, theta);
}
fn dir_at(px: vec2<f32>) -> vec3<f32> { return ray_at(px).d; }
fn uv_of(d: vec3<f32>) -> vec2<f32> {
  return vec2(0.5 + atan2(d.x, -d.z) / TAU, 0.5 - asin(clamp(d.y, -1.0, 1.0)) / PI);
}
fn uv_grad(d: vec3<f32>, dd: vec3<f32>) -> vec2<f32> {  // sphere::equirect::uv_gradient, continuous across the seam
  let r2 = max(d.x * d.x + d.z * d.z, 1e-12);
  return vec2(clamp((-d.z * dd.x + d.x * dd.z) / (r2 * TAU), -0.5, 0.5),  // at a pole half the width is the whole ring
              -dd.y / (PI * sqrt(r2)));
}

@fragment
fn fs_project(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
  let r = ray_at(frag.xy);
  let d = r.d;
  // Analytic central differences of the direction: no derivative builtins, so nothing depends
  // on 2×2 quads or uniform control flow, and the CPU reference computes exactly the same thing.
  let gx = uv_grad(d, dir_at(frag.xy + vec2(0.5, 0.0)) - dir_at(frag.xy - vec2(0.5, 0.0)));
  let gy = uv_grad(d, dir_at(frag.xy + vec2(0.0, 0.5)) - dir_at(frag.xy - vec2(0.0, 0.5)));
  let suv = uv_of(d);
  let uv = suv * src.uv_scale + src.uv_offset;
  let g = mat2x2(gx * src.uv_scale, gy * src.uv_scale);
  let y  = sample_plane(plane0, uv, g, src.size0, src.levels.x);
  let c1 = sample_plane(plane1, uv + src.chroma_offset, g, src.size1, src.levels.y);
  let c2 = sample_plane(plane2, uv + src.chroma_offset, g, src.size1, src.levels.y);
  let s = select_layout(y, c1, c2, src.layout);              // vec4: (y, cb, cr, 1) or (g, b, r, 1), in code / (2ᵇ − 1)
  let rgb = clamp(vec3(dot(src.m0, s), dot(src.m1, s), dot(src.m2, s)), vec3(0.0), vec3(1.0));
  let lin = select(srgb_eotf(rgb), rgb, src.transfer == 1u);
  let inside = r.theta <= PI + 1e-4 && all(suv >= src.cover.xy) && all(suv <= src.cover.zw);
  return vec4(select(vec3(0.0), lin, inside), 1.0);          // a fisheye past θ = π would show the sphere twice
}

// FILTER 0: λ = log2(max(|g·size| over both axes)), clamped to [0, levels − 1], then textureSampleLevel
//           with an anisotropy-1 sampler. FILTER 1: textureSampleGrad(g) with a 16× anisotropic sampler.
// DEBUG_MODE 1 replaces both with textureSample (implicit derivatives of uv), 2 with level 0.
```

Notes:

- **Why central differences.** With them, both the goldens and the CPU
  twin are independent of the GPU's quad layout. The implicit-derivative
  negative control (`DEBUG_MODE 1`) is the only code path that depends on
  quads, which is exactly what the seam tests need. The cost is four extra
  direction evaluations per pixel, which is trivial ALU work.
- **Samplers.** U repeats, so bilinear and anisotropic taps wrap across the
  seam, and V clamps at the poles. A source that covers less than 360°
  horizontally (an sv3d crop) uses a sampler that clamps U too.
- **Isotropic filtering** is what goldens, the CPU comparison and `frame`
  use. 16× anisotropy is implementation-defined, so its output differs
  between lavapipe, RADV and Mesa versions. Isotropic trilinear is softer
  where minification is mild: lodprobe measured 19.58 dB against 24.53 dB
  for level 0 in the seam view. The GUI therefore defaults to the
  anisotropic sampler, as a setting. It needs
  `DownlevelFlags::ANISOTROPIC_FILTERING`, and without it the setting is
  greyed out with a note.
- **Precision.** WGSL allows errors of thousands of ULP in `atan2` and
  `asin`. GPU against CPU error therefore grows toward the image rim
  (lodprobe: 40.8 dB at a tiny planet's rim). Geometry is checked through
  marker colours and per-view calibrated tolerances, never through
  sub-texel positions.
- **The YUV to RGB fold**, `render::color::source_uniform(&ColorInfo,
  &PixelLayout)`, uses H.273's formulas:
  - limited: Y' = (code − 16·2ᵇ⁻⁸) / (219·2ᵇ⁻⁸) and Cb = (code −
    128·2ᵇ⁻⁸) / (224·2ᵇ⁻⁸);
  - full: Y' = code / (2ᵇ − 1) and Cb = (code − 2ᵇ⁻¹) / (2ᵇ − 1);
  - Kr and Kb: 0.2126 and 0.0722 for BT.709, 0.299 and 0.114 for BT.601,
    0.2627 and 0.0593 for BT.2020, 0.212 and 0.087 for SMPTE 240M, 0.30
    and 0.11 for FCC.

  Both texture kinds sample as code / (2ᵇ − 1), so one fold serves both.
  For limited BT.709 it is synth's `from_yuv420` exactly, the shared CPU
  truth.
- **Transfer and the linear target.** M1 linearises SDR with the sRGB EOTF,
  and the encode pass applies the sRGB OETF. An unadjusted view therefore
  reproduces the decoded R'G'B' exactly, as players show it, and an
  export of the same view will match the preview. Filtering happens on
  Y'CbCr codes, as in every player. The linear target exists for M2's
  motion blur and M7's colour work. BT.1886 and scene-referred handling
  are an M7 decision.
- **Stereo.** `st3d` top-bottom and left-right render the left eye only,
  with a notice. A top-bottom source's coarsest mip level can blend one
  row across the eye boundary, which is visible only at that eye's pole
  under extreme minification.

**The encode pass** (`encode.wgsl`) writes either into egui's target
(non-sRGB `Rgba8Unorm` or `Bgra8Unorm` in egui-wgpu 0.37) or into an
offscreen `Rgba8Unorm`. It reads the linear target with `textureLoad` at
`frag.xy − origin`, so pixels map 1:1 with no filtering, and applies the
OETF. A 16-bit PNG reads the `Rgba16Float` target back and applies the
OETF on the CPU in f32. f16 linear storage moves an encoded value by at
most about 2.1e-4, finer than 1/1023.

### 4.4 Upload path

```rust
#[derive(Debug)]
pub struct Uploader { scratch: ScratchPlanes, ingest: Ingest, mips: MipGen, timers: Option<GpuTimers> }
#[derive(Debug, Clone, Copy, Default)]
pub struct UploadStats { pub cpu_ms: f32, pub bytes: u64, pub gpu_ms: Option<f32> }
impl Uploader {
    pub fn new(gpu: &Gpu) -> Self;
    /// Copies the planes in, records ingest and mips, and submits once. Never waits on the GPU.
    pub fn upload(&mut self, gpu: &Gpu, frame: &dyn VideoPlanes, slot: &Slot) -> Result<UploadStats, RenderError>;
    /// device.poll(PollType::Poll): runs map callbacks for timestamps. Never waits.
    pub fn poll(&mut self, gpu: &Gpu);
}
```

What `upload` does for each frame, on the upload thread:

1. **8-bit planes:** `queue.write_texture` into the slot's level 0, with
   `bytes_per_row` set to the frame's stride. The pooled download's
   256-aligned stride makes wgpu-core take its single-copy path. A software
   frame with another stride takes the row-by-row path, which copies the
   same bytes.
2. **Deeper planes:** `write_texture` into the scratch, then the ingest
   pass.
3. The mip passes, with GPU timestamps around steps 2 and 3 when the
   device has `TIMESTAMP_QUERY_INSIDE_ENCODERS`.
4. One encoder and one `queue.submit` per frame, from the upload thread.
5. Marking the slot Ready after `submit` returns, then calling the
   engine's repaint callback.

**Why `write_texture` first.** It is the simplest correct path. Its costs
(a transient staging buffer and per-call bookkeeping, suballocated by
gpu-allocator) are real but unmeasured, so M1 measures before it adds a
belt.

`bench_upload_5k7` (Tier 2, with a Tier 1 smoke run on lavapipe that only
checks it runs) compares `write_texture` with a test-side
`wgpu::util::StagingBelt` path:

- the belt path runs `allocate(size, 256)`, a copy, then
  `copy_buffer_to_texture`, `finish()` and
  `finish_and_recall_on_submit`;
- inputs are 8-bit and 10-bit 5.7K frames and an 8K pair;
- each run covers 300 frames, recording CPU ms, GPU ms and
  `device.generate_allocator_report()` block counts.

**Trigger.** The uploader switches to the belt (inside `Uploader`, with
nothing else changing) if either of these holds on huginn or odin:

- `write_texture`'s CPU time is above 1.5× the belt's, or above 4 ms per
  5.7K frame;
- the allocator report shows blocks allocated and freed per frame.

**The upload ladder:**

- **U1**, always: pooled download, then the cache, then `write_texture` or
  the belt.
- **U2**, only if H1 shows download plus copy above a third of the frame
  budget: the VA surface downloads straight into mapped belt memory, for
  one CPU copy.
- **U3**, PLAN M9: DMA-BUF import, for zero CPU copies.

Slots stay single-plane textures per plane, and level 0 can be filled by a
buffer copy or a texture copy. All three paths therefore fill the same
slot, and the shaders never change.

**Ordering without retirement callbacks.** The upload thread writes only
into a slot that is `Free` and whose `Arc<GpuFrame>` has a strong count of
1. That means no `ViewerCallback` in egui's shape list still holds it, and
egui drops those only after eframe's submit. Every draw that sampled the
slot was therefore submitted before the upload's write. wgpu orders
submissions on one queue and inserts the barriers between them, so no
`on_submitted_work_done` bookkeeping is needed.

### 4.5 Public API, the CPU reference and checks

```rust
#[derive(Debug, Clone, Copy, PartialEq)] pub struct ViewParams { pub orientation: glam::DQuat, pub lens: spherecut_sphere::perspective::Lens }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Filter { Isotropic, Anisotropic }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum DebugMode { Off, ImplicitLod, Level0, LodHeatmap }

pub struct Projector;     // pipelines per (Filter, DebugMode), built on demand; layouts; samplers
impl Projector { pub fn new(gpu: &Gpu) -> Self;
  pub fn record(&self, enc: &mut wgpu::CommandEncoder, slot: &GpuFrame, view: &ViewParams, target: &LinearTarget,
                filter: Filter, debug: DebugMode, timers: Option<&mut GpuTimers>); }
pub struct LinearTarget;  // Rgba16Float; reallocated only when the viewer outgrows it
pub struct Encoder;       // one pipeline per output format
impl Encoder { pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, target: &LinearTarget, origin_px: [u32; 2]); }

/// One frame, headless: upload, project, encode, read back. `spherecut frame` and the goldens use it.
pub fn render_still(gpu: &Gpu, frame: &dyn VideoPlanes, view: &ViewParams, size: (u32, u32),
                    opts: StillOptions /* filter, depth: Eight | Sixteen */) -> Result<Image, RenderError>;

pub mod reference {            // f64, safe, no GPU
    pub fn sphere_dir(view: &ViewParams, size: (u32, u32), x: f64, y: f64) -> DVec3;
    pub fn uv_footprint(view: &ViewParams, size: (u32, u32), i: u32, j: u32) -> (DVec2, DVec2);
    /// Emulates FILTER 0 exactly: the same central differences, λ, area mips and bilinear taps, in f64.
    pub fn render_trilinear(frame: &dyn VideoPlanes, view: &ViewParams, size: (u32, u32)) -> ImageF64;
    pub fn render_level0(frame: &dyn VideoPlanes, view: &ViewParams, size: (u32, u32)) -> ImageF64;
}
```

New in sphere:

- `pixel::{to_lens, from_lens}`, with pixel centres at i + 0.5;
- `equirect::uv_gradient(d, dd)`;
- `Lens::project_morph(d)`, the inverse of `unproject` for any morph,
  found by bisection on the monotonic blended θ(ρ).

synth gains `render_view_ideal(pattern, view_fn, size, n)`, the
supersampled analytic truth. It lives in synth so that render stays free
of synth.

Every pipeline (all `FILTER` × `DEBUG_MODE` combinations, ingest, mips and
encode) is created in a Tier 1 test on lavapipe, which validates the WGSL.
A test with `naga =30.0.1` as a dev-dependency (already in the tree
through wgpu) parses `project.wgsl` and checks each uniform member's
offset and each struct's size against `offset_of!` and `size_of` on the
Rust side. A validating driver does not catch that mismatch.

## 5. `spherecut frame`

```
spherecut frame <FILE> -o <OUT.png>
    [--time <T> | --frame <N>]     T: "12", "12.5", "1:02.5", "01:02:03.250", "30000/1001", or "370f"; default 0
    [--yaw <DEG>] [--pitch <DEG>] [--roll <DEG>]   default 0 (D31)
    [--fov <DEG>]                  horizontal, default 100, at most 170 / 360 / 340 for rect / fisheye / stereo
    [--projection rectilinear|fisheye|stereographic]   default rectilinear
    [--morph-to <PROJ> --morph <0..1>]
    [--size <W>x<H>]               default 1920x1080; each side 16..=16384 and within the adapter limit
    [--depth 8|16]                 PNG bit depth, default 8
    [--filter iso|aniso]           default iso, the same on every driver (D30)
    [--decoder auto|software|vaapi]   default auto
    [--render-node <PATH>]         default: the node of the chosen adapter (section 3.5)
    [--adapter auto|cpu|<NAME>]    default auto: a hardware Vulkan adapter, else lavapipe
    [--equirect]                   treat an untagged or non-2:1 file as equirect
    [--stream <N>]
    [--reference]                  render with the f64 CPU reference instead of the GPU
    [--overwrite] [--json]
```

**Behaviour:**

1. **Parse.** `TimeSpec::from_str` in core reads decimals exactly ("12.345"
   is 12345/1000, never through f64), accepts h:m:s, rationals and an `f`
   suffix for a frame number, and refuses negatives and junk. `--frame N`
   is sugar for `--time Nf`. Usage errors exit 2.
2. **Check the output** before any work. An existing output is refused
   with `output_exists` (exit 1) unless `--overwrite` is given. A
   directory, pipe or device there is refused either way (D16).
3. **Install the SIGINT handler** (`cli::interrupt`). The work runs on a
   worker thread. The main thread polls the handler's flag every 10 ms,
   and when it is set, it calls `StopSource::shutdown()` and raises the
   AVIO interrupt.
4. **Probe**, through the confinement. A layout other than equirect is
   refused with `not_equirect` (exit 1), naming what was found ("Insta360
   dual fisheye: stitching arrives in M3"), unless `--equirect` is given.
   Other cases are warnings:
   - stereo renders the left eye;
   - an sv3d crop is applied;
   - a non-zero sv3d pose is "not applied (M2)".
5. **Choose the GPU.** No Vulkan adapter is `no_gpu` (exit 3), and a
   `--adapter` name that matches none is `adapter_not_found` (exit 3).
6. **Open the decoder.**
   - `auto` tries VA-API on the adapter's render node and falls back with a
     note. On a box with no render node it decodes in software with the
     note "no VA-API device".
   - `--decoder vaapi` turns a missing device into `hw_unavailable` and a
     refusal into `hw_unsupported`, both exit 3.
   - A codec this FFmpeg can't decode is `no_decoder`, exit 3.
7. **Seek** with `Exact`.
   - `AfterLast` is `out_of_range` (exit 1), naming the range: "no frame
     at 99 s: the clip runs from 0 to 9.967 s".
   - A target past a damage point is `truncated` (exit 1), naming the last
     good time.
   - Damage after the target is a warning.
8. **Render** with `render_still`, or with `reference::render_trilinear`
   for `--reference`.
9. **Write** a PNG with `image`'s `PngEncoder` (`Rgb8` or `Rgb16`; alpha is
   always 1, so it is dropped) into a `TempFile` next to the output. Then:
   - `persist_new` publishes it with `std::fs::hard_link(tmp, out)`, which
     fails atomically with EEXIST if something appeared meanwhile, and
     removes the temp file;
   - on a filesystem without hard links, it falls back to `create_new` on
     the output, a copy and `sync_all`;
   - `--overwrite` uses `persist_replace`, D16's rename.
10. **Report.** Without `--json`, stdout gets one line: "wrote out.png:
    1920×1080 rectilinear 100° at 62.233 s (frame 1867, pts 186700),
    decoded on VA-API, rendered on AMD Radeon 8060S (RADV)". Warnings go to
    stderr through tracing.

**Exit codes** (each tested in `crates/cli/tests/contract.rs`):

| Case | Exit | code |
|---|---:|---|
| written | 0 | |
| bad size, projection, fov, time syntax, unknown flag (`--json` still gets an envelope) | 2 | `usage` |
| missing or unreadable file, damaged at the target, not equirect, past the end, output exists | 1 | `not_found`, `truncated`, `not_equirect`, `out_of_range`, `output_exists`, ... |
| no Vulkan adapter, unknown adapter, `--decoder vaapi` without a working device, no decoder for the codec | 3 | `no_gpu`, `adapter_not_found`, `hw_unavailable`, `hw_unsupported`, `no_decoder` |
| SIGINT | 4 | `cancelled` (no output file; a second Ctrl+C kills at once, through `SA_RESETHAND`) |

**`--json` result** (section 5.1 gives the envelope):

- `output`;
- `frame`, with `pts` and `time`. `time` is `{exact, seconds, frame}`,
  where `frame` is present for CFR streams, or when the frame was reached
  by counting;
- `requested` (a `{exact, seconds}`) and `exactness`;
- `size`;
- `view`: `yaw`, `pitch`, `roll`, `fov`, `projection`, `morph_to`,
  `morph`;
- `filter`;
- `decode`: `path`, plus `fallback` when there was one;
- `gpu`: `name`, `driver`, `driver_info`, `device_type`;
- `color`: the resolved `ColorInfo` with its `assumed` flags;
- `source`: `codec`, `size`, `pix_fmt`.

### 5.1 The JSON contract (probe, doctor, frame)

`crates/cli/src/contract.rs` holds the envelope. A1's `spherecut call`
and `spherecut mcp` reuse it.

```rust
#[derive(Debug, serde::Serialize)]
pub struct Envelope<T: serde::Serialize> {
    pub schema: &'static str,      // "spherecut.probe" | "spherecut.doctor" | "spherecut.frame" | "spherecut" (usage before a subcommand)
    pub version: u32,              // 1
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")] pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<ErrorBody>,
    pub warnings: Vec<Warning>,    // { code, message }
}
#[derive(Debug, serde::Serialize)]
pub struct ErrorBody { pub code: &'static str, pub message: String,
                       #[serde(skip_serializing_if = "Option::is_none")] pub path: Option<String>,
                       #[serde(skip_serializing_if = "Vec::is_empty")] pub ffmpeg_log: Vec<String> }
pub enum Exit { Ok = 0, Failed = 1, Usage = 2, Missing = 3, Cancelled = 4 }
```

The rules:

- **One document per run.** With `--json`, stdout carries exactly one
  compact document per run (`serde_json::to_writer`, then `\n`), on
  success and on failure. stderr carries only logs.
- **Usage errors** go through `Cli::try_parse()`. When argv contains
  `--json`, the CLI prints an envelope with code `usage` and exits 2.
  `--help` and `--version` print text and exit 0.
- **The same shape for one file or many.** probe's `result` is always
  `{"files": [{"path", "ok", "report"?, "error"?}]}`, and the top-level
  `ok` is true only when every file is ok.
- **Times.** `core::Time` serialises as `{"exact": "1001/30000",
  "seconds": 0.0333...}`. A time that is a video frame adds `"frame"`,
  through `FrameTime`. `Rational` serialises as `"30000/1001"`, or `"25"`
  for an integer. Both read back from the new form. The ron test in core
  is updated to the new form. probe's `Container.duration`, `start_time`
  and `StreamInfo.duration` become `Time`. Time bases and frame rates stay
  `Rational`.
- **Error codes** come from three places:
  - `MediaError::code()`;
  - probe's damage codes (`damaged`, `truncated`, `not_media`,
    `unsupported_container`);
  - CLI codes (`usage`, `output_exists`, `not_equirect`, `out_of_range`,
    `no_gpu`, `adapter_not_found`, `no_display`, `cancelled`,
    `internal`).

  `docs/cli-json.md` lists them all, with the exit code of each, and A1's
  `docs/agents.md` will link it.
- **Exit codes per command:**
  - **probe:** 0 when every file is ok; 1 when any file failed or was
    refused; 2 for usage. A missing decoder stays a warning, as M0 has it,
    so probe never exits 3.
  - **doctor:** 0 with no errors and 1 with any error, missing H.264,
    HEVC, ProRes or AAC decoders included (D18 and the README); 2 for
    usage. Doctor checks capabilities and reports them, so it never exits
    3 for its own findings.
  - **frame:** the table above.

## 6. Golden images

**Where:** `crates/render/tests/goldens.rs`, with references in
`crates/render/tests/goldens/*.png` and `goldens/manifest.json`. A second,
smaller set in `crates/cli/tests/frame.rs` goes from decode to render
through real lossless clips.

**The manifest** records, for each image:

- the FNV-64 hash of the source planes;
- Mesa's `driver_info` and the LLVM version;
- the wgpu version;
- the commit and the date;
- the view;
- the thresholds, including the calibrated CPU tolerance;
- the command that regenerates the image.

**Comparison rules:**

| Against | Rule | Why |
|---|---|---|
| the stored PNG, same `driver_info` and source hash | identical bytes | lavapipe is deterministic at a fixed Mesa build (3 of 3 runs identical) |
| the stored PNG, another driver build | PSNR ≥ 48 dB, SSIM ≥ 0.995, max ≤ 4 LSB | 48.13 dB is an RMS error of 1 LSB, which absorbs Mesa or LLVM rounding, while a half-pixel shift or a 0.5 LOD bias falls below 40 dB on these patterns |
| a source-hash mismatch | reported as "the synthetic source changed" (libm drift, a synth edit), never passed as a render difference | keeps synth drift and render drift apart |
| `reference::render_trilinear` (f64) | PSNR ≥ the per-view tolerance, calibrated when blessing as measured − 3 dB and never below 36 dB | lodprobe measured 40.8 to 55.8 dB, with the error growing toward the rim (section 4.3) |
| `render_view_ideal` (minified views) | a per-view PSNR floor, plus a margin of at least 3 dB over the `DEBUG_MODE 2` (level 0) control | proves the test can see aliasing |

**Blessing** is explicit. `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p
spherecut-render --test goldens` rewrites the PNGs and the manifest only
when two things hold:

- every image passes its CPU-reference tolerance;
- every negative control fails as it should.

Otherwise it refuses and lists what failed. Failing runs write diff
images to `target/golden-diffs/`. The harness asserts `DeviceType::Cpu`
and puts `driver_info` into every failure message. Goldens use
`Filter::Isotropic` only.

**Metrics** live in `spherecut-testkit::compare`, which still has no
dependencies:

- `psnr` over RGB (8-bit, 16-bit and f64);
- `ssim` on BT.709 luma, with an 11×11 Gaussian (σ 1.5);
- `diff_stats(a, b, threshold) -> {max, count_over, where}`;
- `fnv64`.

They have unit tests with known values. `dssim` was rejected because it is
AGPL-3.0, and `image-compare` because it pulls in rayon for two functions.

**Sizes.** Sources are 1024×512, or 2048×1024 for the high-frequency
checker. Outputs are 256×144, 192×192, or 256×256 for the tiny planet.
The whole set is about 0.5 MB.

| Test | Source | View | Checks |
|---|---|---|---|
| `rect_forward` | grid, 8-bit limited | rect 90°, identity | stored; CPU |
| `rect_ypr` | grid | rect 90°, yaw 30, pitch 20, roll 10 | stored; CPU; marker colours at known pixels |
| `fisheye_180`, `fisheye_360` | grid | fisheye | stored; CPU; black past θ = π |
| `stereo_tinyplanet` | grid | stereographic 300°, pitch −90 | stored; CPU; ideal |
| `morph_half` | grid | rect to stereographic, morph 0.5, 150° | stored; CPU |
| `seam_quad` | high-frequency checker, 2048×1024 R8 | rect 90°, yaw chosen so lon ±180° falls between pixel centres 2k + 0.5 and 2k + 1.5, inside a 2×2 quad; the CPU computes the yaw and asserts the placement before rendering | `diff_stats` against its roll twin (the texture rolled by W/2 and the source rotated 180°, so the two must match): 0 pixels over 2/255. Negative control: `DEBUG_MODE 1` gives over 100 bad pixels (lodprobe: 504) |
| `zenith_quad`, `nadir_quad` | the same checker | rect 120°, pitch ±90, the pole inside a quad | the roll twin and the same control (lodprobe: 344 without analytic LOD) |
| `tinyplanet_pole` | the same checker | stereographic 300°, pitch −90 | the roll twin and the control (lodprobe: 192) |
| `shimmer_tinyplanet`, `shimmer_fisheye360` | the checker | 8 frames yawing 0.37° per frame | frame-to-frame RMS change at most the ideal's plus 2 LSB; the level-0 control must exceed the ideal's by at least 3×; PSNR against the ideal at least the control's plus 3 dB |
| `tenbit_ramp` | a 10-bit horizontal ramp, as P010 and as `yuv420p10` | rect 20°, magnified, 16-bit readback | both layouts byte-identical; monotonic non-decreasing along the ramp; within 2/1023 of the CPU |
| `layout_invariance` | one picture as NV12 and as I420, with strides padded to 64, 256 and 4109 bytes | rect 90° | byte-identical |
| `passthrough_exact` | flat fields at Y = k, full range, and at 16 to 235, limited | identity view filling the target | full: output byte = k for every k; limited: exactly round((k − 16) · 255 / 219), which never ties |
| `range_full_vs_limited` | one scene, both ranges | rect 90° | at most 2 LSB apart; control: decoding full as limited differs by over 10 LSB |
| `matrix_601_709` | BT.601- and BT.709-tagged | rect 90° | each matches the CPU; control: 601 decoded as 709 differs |
| `siting_left_center` | a sharp vertical chroma edge | rect 20°, magnified | Left and Center each within 1 LSB of the CPU; control: Left decoded as Center differs |
| `crop_half` | sv3d bounds: a 180° panorama | rect 120° | black outside the cover; CPU |
| `lod_heatmap` | grid | fisheye 270° | `DEBUG_MODE 3` levels within ±0.5 of the CPU's `uv_footprint` |
| `mips_area` | odd sizes (45 to 22, 23 to 11) | none | each GPU level within 1 LSB of the CPU area filter, for R8, Rg8, R16Float and Rg16Float |

Why `diff_stats` and not PSNR for the seam and the poles: a line of 504 bad
pixels in 65 536 barely moves PSNR. The bad-pixel count is what the test
is about.

**Pipeline goldens** (`crates/cli/tests/frame.rs`) cover:

- a lossless 10-bit HEVC clip, decoded as `yuv420p10le`;
- a full-range H.264 clip;
- an NV12 decode against a `yuv420p` decode of the same picture
  (identical bytes);
- a 4:2:2 10-bit clip at full chroma resolution;
- **the swscale cross-check.** 4:4:4 sources at 601 and 709, limited and
  full, 8 and 10 bits, are converted to RGB by swscale with the matrix and
  range passed explicitly through `sws_setColorspaceDetails` (swscale
  would assume BT.601 for an untagged clip). They are compared with an
  identity view at level 0, within 1 LSB. 4:4:4 keeps swscale's chroma
  upsampling out of the comparison.

**Tier 2:**

- `goldens_on_gpu_match_reference` runs the isotropic set on RADV against
  the CPU and ideal references only, never against the lavapipe PNGs.
- `goldens_on_host_llvmpipe` runs the stored goldens on huginn's
  llvmpipe (Mesa 26.2.5, LLVM 22) and records the drift.

## 7. GUI (`spherecut-app`)

### 7.1 Structure and eframe setup

```rust
pub struct SpherecutApp {
    gpu: Arc<Gpu>,                   // eframe's device, wrapped with Gpu::from_parts
    slots: Arc<SlotRing>,            // shared with the upload thread; never inside CallbackResources
    player: Option<Player>,          // never blocks (section 8)
    tree: egui_tiles::Tree<Pane>,    // persisted
    view: ViewState,                 // orientation (DQuat), lens, morph animation, filter
    drag: Option<GrabStart>,
    overlay: OverlayState,
    doctor: DoctorState,             // Pending(one-shot receiver, try_recv) | Done(Arc<DoctorReport>)
    notices: Notices,
    files: FilesPane,                // directory listing on a worker
    settings: Settings,              // cache MiB, decoder, filter, slots, A/V offset, overlay
    smoke: Option<SmokeDriver>,      // `--smoke`
}
impl SpherecutApp { pub fn ui(&mut self, ui: &mut egui::Ui); }    // all UI; no eframe types, so kittest drives it
impl eframe::App for SpherecutApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame); // player events, present, notices
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame);      // calls SpherecutApp::ui
    fn save(&mut self, storage: &mut dyn eframe::Storage);
    fn on_exit(&mut self);                                               // player.begin_shutdown(): never waits
}
pub fn run(args: GuiArgs) -> anyhow::Result<()>; // the CLI calls this when there is no subcommand
```

**NativeOptions:**

- `renderer: Wgpu`, with `multisampling: 0` and `depth_buffer: 0`. Our
  pipelines in `paint` must match egui's render pass, which then has one
  sample and no depth. egui-wgpu's `RenderState` has no MSAA field, so
  this setting is where the sample count comes from.
- `wgpu_options.wgpu_setup = WgpuSetup::CreateNew(WgpuSetupCreateNew {
  instance_descriptor: Gpu::instance_descriptor(), native_adapter_selector:
  Gpu::select_adapter, device_descriptor: Gpu::device_descriptor, .. })`.
  The surface configuration stays at egui-wgpu's default.
- `viewport = ViewportBuilder::default().with_app_id(brand::APP_ID)
  .with_decorations(true).with_min_inner_size([480.0, 320.0])`.

**Startup:**

- In the creation callback, the app wraps `cc.wgpu_render_state` in
  `Gpu::from_parts`.
- It inserts `ViewerResources` into
  `render_state.renderer.write().callback_resources`, once, on the UI
  thread, before the first frame, so the lock is uncontended.
  `ViewerResources` holds the `Projector`, the `Encoder` for
  `target_format`, the `LinearTarget` and the timestamp ring.
- The upload thread never touches egui's renderer lock.

**The CLI:**

- `Cli.cmd` becomes `Option<Cmd>`, with `args_conflicts_with_subcommands`,
  positional `files`, `--cache-mb`, `--adapter` and a hidden `--smoke`. So
  `spherecut` and `spherecut clip.mp4` open the GUI, and `spherecut probe
  ...` is unchanged.
- The GUI installs the same tracing subscriber and calls
  `sync_ffmpeg_log_level()`.
- With no Wayland or X11 display, the GUI exits 3 with `no_display` and a
  message.

**Shutdown.** `on_exit` calls `begin_shutdown()`, which sets flags and
notifies condvars and never waits. The threads are joined after
`run_native` returns, outside the event loop, with a 2 s budget. A thread
still stuck in a driver call is logged and detached.

### 7.2 Layout (egui_tiles 0.18)

```
┌ File  View  Help ───────────────────────────────────── [notice banners, dismissible] ┐
│┌ Viewer ───────────────────────────────────────────┐┌ Info │ Doctor │ Files ─────────┐│
││ [overlay, F3]                                     ││ probe report, colour as coded  ││
││              paint callback (projection)          ││ and as resolved (assumed       ││
││ ▶ ⏮ ⏭  00:01:02.250 / 00:10:00.000   #1867        ││ fields marked), spherical,     ││
││ [scrub bar, cached ranges shaded]                 ││ camera, decode path            ││
││ Rect | Fisheye | Tiny planet   FOV 100°  Y P R  ⟲ ││                                ││
│└───────────────────────────────────────────────────┘└────────────────────────────────┘│
└───────────────────────────────────────────────────────────────────────────────────────┘
```

- The tree is `Tree::new_horizontal`, split 3 : 1. The panes are Viewer,
  Info, Doctor and Files, and eframe persistence saves the tree
  (egui_tiles' default `serde` feature).
- The transport bar belongs to the viewer pane, so it moves with it. The
  scrub bar shades the cached ranges from `PlayerSnapshot.cache`.
- The Files pane replaces a file dialog. It lists a directory on a worker
  thread, filters to video extensions and remembers the last directory.
  `rfd` is not used: its portal backend pulls in zbus (the
  RUSTSEC-2026-0370 family), and its GTK backend pulls in GTK 3.

### 7.3 The viewer paint callback

```rust
struct ViewerCallback {
    rect: egui::Rect,                // the viewer rect in points, set in ui()
    frame: Option<Arc<GpuFrame>>,    // the slot to show (section 8.3)
    view: ViewParams, filter: Filter, debug: DebugMode,
    redraw: bool,                    // false when the frame and view are unchanged: encode only
    timers: Arc<GpuTimerShared>,
}
impl egui_wgpu::CallbackTrait for ViewerCallback {
    fn prepare(&self, _device: &wgpu::Device, _queue: &wgpu::Queue, sd: &egui_wgpu::ScreenDescriptor,
               enc: &mut wgpu::CommandEncoder, res: &mut egui_wgpu::CallbackResources) -> Vec<wgpu::CommandBuffer> {
        // The same rounding paint() gets, from the real surface size (never [u32::MAX; 2], which panics):
        let vp = egui::epaint::ViewportInPixels::from_points(&self.rect, sd.pixels_per_point, sd.size_in_pixels);
        // Resize the LinearTarget to vp's size if needed; if `redraw`, record the projection
        // (with timestamp writes) into `enc`. Records commands only; never waits.
        Vec::new()
    }
    fn paint(&self, info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>,
             res: &egui_wgpu::CallbackResources) {
        // Encoder::draw: textureLoad(linear, frag.xy − info.viewport_in_pixels() origin), then the sRGB OETF.
    }
}
```

- **One pixel per pixel.** The linear target has the callback's physical
  size, so fractional scaling never resamples the picture.
- **Input uses the same rounding.** `ui()` maps the pointer into physical
  pixels with `from_points` and the window's size in pixels. That agrees
  with `prepare`'s rectangle whenever the viewer lies inside the window,
  which tiles guarantee. A kittest test asserts the two agree at ppp 1.25
  and 1.5.
- **Unchanged frames skip the projection**, so a 30 fps video on a 144 Hz
  display projects 30 times a second, not 144.
- **A callback, not `register_native_texture`.** It avoids an extra copy
  and texture, and it keeps the GPU work inside egui's submit.
- `Debug` is implemented by hand, because the workspace warns on
  `missing_debug_implementations` and clippy runs with `-D warnings`.

### 7.4 Grab, zoom and the projection switch

**The convention** (`sphere::orient`, D31):

- yaw > 0 turns right, from −Z toward +X;
- pitch > 0 looks up;
- roll > 0 rolls the camera clockwise as the viewer sees it, so the
  horizon appears to turn counter-clockwise;
- q = R_y(−yaw) · R_x(pitch) · R_z(−roll), with world = q · view;
- at |pitch| = 90°, `to_ypr` puts all the heading into yaw.

```rust
pub mod grab {
    #[derive(Debug, Clone, Copy)] pub struct GrabStart { pub world: DVec3, pub roll: f64 }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum GrabFit { Exact, Clamped }
    pub fn start(q: DQuat, lens: &Lens, p0: DVec2) -> GrabStart;          // world = q · unproject(p0)
    /// Level mode: keeps the roll and puts `world` under lens point p (Clamped when no
    /// orientation with that roll can, near a pole: the closest one).
    pub fn solve_level(start: &GrabStart, lens: &Lens, p: DVec2) -> (DQuat, GrabFit);
    /// Free mode (Ctrl held): the minimal rotation, q' = from_rotation_arc(q · unproject(p), world) · q.
    pub fn solve_free(q: DQuat, start: &GrabStart, lens: &Lens, p: DVec2) -> DQuat;
    /// Zoom about the cursor: the direction under p stays under p when the lens changes.
    pub fn zoom_about(q: DQuat, old: &Lens, new: &Lens, p: DVec2) -> DQuat;
}
```

**Level mode.** Let v' = R_z(−roll) · unproject(p) and w = world. We
solve R_y(−yaw) · R_x(pitch) · v' = w:

- **Pitch.** R_y leaves y unchanged, so v'.y cos p − v'.z sin p = w.y,
  which is R cos(p + φ) = w.y. Take the root nearest the current pitch,
  clamped to ±90°.
- **Yaw** is the azimuth difference.
- **No root** (|w.y| > R): take the best pitch, and the fit is `Clamped`.

**Pointer to lens point.** The pointer comes in points. It is multiplied
by ppp, offset by the rounded viewport origin, and mapped with
`sphere::pixel::to_lens` over the viewport's pixel size, the same
pixel-centre convention the shader uses.

**Tests:**

- a proptest of 10 000 cases over random yaw, pitch, roll, projection,
  morph, hfov, p0 and p:
  - when `Exact`, the angle between q1 · unproject(p) and w is under 1e-9
    rad, and `project_morph(q1⁻¹ · w)` is within 1e-6 lens units of p;
  - when `Clamped`, no pitch on a 0.01° grid around the result gets
    closer;
  - roll is kept to 1e-12;
- the kittest marker drag at ppp 1.25: press on the red F marker's
  centroid, move and release. The marker's centroid in the rendered image
  must land within 0.5 physical pixels of the release point, and the
  app's q must equal `solve_level`'s to 1e-9.

**Zoom:**

- Scrolling sets hfov ← hfov · exp(−0.0015 · scroll_y_points), clamped
  to [10°, the projection's maximum].
- `zoom_delta` (Ctrl+scroll, touchscreen pinch) sets hfov ← hfov /
  zoom_delta.
- After each change, `zoom_about` keeps the direction under the cursor
  fixed.
- **Touchpad pinch does not arrive on Wayland or X11.** winit 0.30.13
  delivers `PinchGesture` on macOS and iOS only, so two-finger scrolling
  and Ctrl+scroll are the touchpad's zoom. This is recorded in D34 and in
  PROGRESS, and revisited when eframe moves to a winit with Wayland
  pointer gestures.

**The projection switch.** The Rect, Fisheye and Tiny planet buttons (or
keys 1, 2, 3) animate `Lens { from: current, to: target, morph: 0 → 1 }`
over 250 ms with smoothstep. Then from = to and morph = 0. Rectilinear's
hfov is eased down to at most 170°. The Tiny planet preset is
stereographic 300° at pitch −90°. A morph slider in the View menu exposes
the morph directly.

### 7.5 Transport

| Control | Keys | Action |
|---|---|---|
| play / pause | Space, K | toggle |
| step forward | → or `.` | the cached successor (instant), else a decode |
| step back | ← or `,` | `seek(pts − 1 tick, Exact)`; the cached predecessor is instant |
| jump 1 s | Shift+← / Shift+→ | `seek(±1 s, Exact)` |
| start, end | Home, End | the first and last frames |
| scrub | drag the bar | `Seek{Keyframe}` with at most one in flight and the latest winning; `Seek{Exact}` 150 ms after the pointer stops, and on release |
| reset view | R | yaw = pitch = roll = 0 |
| zoom | + / − | FOV in steps |
| overlay | F3 | toggle |

Audio is muted while scrubbing. Audio scrubbing is M5, with JKL shuttle.

### 7.6 The startup doctor check

- `media::doctor::run(render_node)` runs on a worker at startup, with the
  render node of the chosen adapter (section 3.5). It takes about 1 to 3
  s. A file opens in parallel, because the fallback in section 3.5
  already handles a device that refuses.
- Each Error or Warning `Issue` becomes a banner with "what", "impact" and
  the fix commands, with a copy button (eframe's `clipboard` feature).
  Info issues appear only in the Doctor pane, which shows the whole
  report.
- The text formatter moves from `cli/src/show.rs` to
  `media::doctor::render_text`, so the CLI and the GUI print the same
  words.
- The app adds its own issues:
  - a CPU adapter (Warning: "playback will be slow", with the Vulkan fix);
  - no audio device (Info);
  - no anisotropic filtering (Info).

### 7.7 HiDPI, fractional scaling, GNOME and Hyprland

- **Pixels.** Physical sizes come only from `ViewportInPixels` and
  `pixels_per_point`, and nothing assumes an integer scale.
- **Tests (kittest on lavapipe):**
  - at ppp 1.0, 1.25, 1.5 and 2.0, the viewer's crop of the harness render
    must be byte-identical to `render_still` at the same pixel size, with
    the same view and filter. That proves there is no resampling and that
    the pixel mapping is right at fractional scales;
  - a resize sweep from 1×1 and 64×48 up to 3840×2160 must never panic.
- **Wayland fractional scaling.** winit binds `wp_fractional_scale_v1` and
  `wp_viewporter`. hw-test records `gsettings get org.gnome.mutter
  experimental-features`, and the smoke run takes screenshots at the
  session's scale. Pat runs it at 100% and at a fractional scale.
- **Decorations.**
  - `with_decorations(true)`, and Spherecut never draws its own title bar.
  - GNOME gets sctk's plain fallback frame, because winit's
    `wayland-csd-adwaita` stays off (RUSTSEC-2026-0192 through
    ttf-parser). Pat judges it from the screenshots (section 16).
  - Hyprland draws its own borders, and window rules can match `class =
    io.github.cyclonesurfer.Spherecut`.
- **No file drop on Wayland.** winit 0.30.13 delivers `DroppedFile` only on
  X11. The CLI argument and the Files pane cover opening files.
- **Occlusion.** eframe 0.37 keeps running when Wayland frame callbacks
  stop for an occluded window. Playback follows the clock, never
  `request_repaint`, so nothing breaks. Drops are not counted while no UI
  frames are painted (a gap over 100 ms).

### 7.8 Notices

Notices are a non-modal stack of banners, deduplicated by kind:

- VA-API fallback, with its reason;
- no audio stream, or no audio device;
- a CPU adapter;
- a non-equirect source, shown flat in a raw view with "stitching arrives
  in M3";
- stereo: showing the left eye;
- HDR or BT.2020;
- an sv3d pose present but not applied;
- colour fields assumed;
- a frame cache too small for this source;
- the doctor's issues.

### 7.9 `crates/app/clippy.toml`

A crate's `clippy.toml` replaces the root one, takes exact paths only (no
wildcards) and cannot be scoped to modules. So it:

- repeats `allow-unwrap-in-tests`, `allow-expect-in-tests` and the
  `std::net` `disallowed-types`;
- adds `disallowed-methods`, each with the reason "the UI thread must not
  block (PLAN M1)":
  - `std::sync::Mutex::lock`;
  - `std::sync::RwLock::read` and `std::sync::RwLock::write`;
  - `epaint::mutex::Mutex::lock`, `epaint::mutex::RwLock::read` and
    `epaint::mutex::RwLock::write`. These are egui's own parking_lot
    wrappers (re-exported as `egui::mutex`), and egui-wgpu 0.37's
    `RenderState.renderer` is an `Arc<epaint::mutex::RwLock<Renderer>>`,
    so the std entries alone would not see the one startup write below
    and its `#[expect]` would go unfulfilled;
  - `std::sync::Condvar::wait`, `std::sync::Condvar::wait_while` and
    `std::sync::Condvar::wait_timeout`;
  - `std::sync::mpsc::Receiver::recv`;
  - `std::thread::JoinHandle::join`;
  - `std::thread::sleep`;
  - `std::fs::read`, `std::fs::read_to_string`, `std::fs::read_dir` and
    `std::fs::File::open`;
  - `wgpu::Device::poll`;
  - `pollster::block_on`.

The app's few worker functions (the doctor runner, the directory listing,
the post-`run_native` join, the one-time startup insert into
`callback_resources`) carry `#[expect(clippy::disallowed_methods, reason
= "...")]` with the reason spelled out. The runtime guard in section 8.6
covers other crates' code called from the UI.

## 8. Threads and frame cache (`spherecut-playback`)

### 8.1 Threads

| Thread | Crate | May block on | Talks through |
|---|---|---|---|
| UI (eframe main) | app | vsync present only | `Player` commands (`mpsc::Sender`, unbounded, never blocks); `try_lock` and atomics for snapshots and slots |
| `decode-v{i}`, one per video stream | playback | demux, decode, cache back-pressure | `FrameCache` (`Mutex` + `Condvar`), its command channel, `StopToken` |
| libavcodec frame threads (software only) | FFmpeg | | |
| `upload` | playback | a free slot or a needed frame (`Condvar`, 20 ms timeout so it can poll) | `FrameCache`, `SlotRing`, the wgpu queue |
| `audio-decode` | playback | decode; sleeps 5 ms while the ring is full | the chunk ring, its command channel |
| `audio-out` | audio | owns the cpal stream: build, play, pause, rebuild after device loss | its command channel |
| cpal callback (real time) | audio | nothing: no locks, no allocation, no syscalls | the chunk ring, the clock seqlock |
| `open`, `doctor`, `dir-list` (transient) | app, playback | file I/O | one-shot channels read with `try_recv` |

The cpal stream is created, paused and dropped only on `audio-out`, never
on the UI thread. A Pulse server round trip therefore can't stall a frame.
A seek never rebuilds the stream, because it flushes through the ring's
epoch (section 9.2).

### 8.2 The Player API

```rust
pub struct Player { /* command sender, shared snapshot, threads */ }
impl Player {
    pub fn new(backend: Arc<dyn MediaBackend>, gpu: Arc<Gpu>, slots: Arc<SlotRing>,
               audio: Arc<dyn AudioOutputFactory>, config: PlayerConfig,
               repaint: Arc<dyn Fn() + Send + Sync>) -> Player;
    pub fn open(&self, path: PathBuf);                        // all commands return at once
    pub fn play(&self); pub fn pause(&self);
    pub fn seek(&self, at: Time, kind: SeekKind);             // Exact | Keyframe
    pub fn step(&self, frames: i32);
    pub fn set_config(&self, config: PlayerConfig);
    /// UI thread, each frame: picks the slot to show for clock.time_at(now + present_latency) + av_offset.
    pub fn present(&self, now: Instant) -> Option<Arc<GpuFrame>>;
    pub fn snapshot(&self) -> PlayerSnapshot;                 // try_lock; on contention, the previous copy
    pub fn events(&self) -> Vec<PlayerEvent>;                 // try_recv until empty
    pub fn begin_shutdown(&self);                             // never waits
    pub fn join(self, budget: Duration) -> ShutdownReport;    // off the UI thread
}

#[derive(Debug, Clone)]
pub struct PlayerConfig {
    pub cache: CacheBudget,          // Auto | Bytes(u64): Settings, --cache-mb, SPHERECUT_FRAME_CACHE_MIB
    pub ahead: Duration,             // decode-ahead target, default 1.0 s
    pub gpu_slots: u8,               // 3..=6, default 4
    pub decoder: DecoderPreference,  // Auto | VaApiOnly | Software
    pub av_offset_ms: i32,
}
```

The same `Player` runs headless. The Tier 2 real-time bench and M2's
export drive it with an offscreen target and a `SystemClock` or a
frame-stepped `ManualClock`, with no eframe.

### 8.3 The bounded frame cache, and GPU slots

```rust
pub fn auto_budget(meminfo: &str, cgroup_max: Option<&str>, cgroup_current: Option<&str>) -> u64;
```

**The budget** (Auto):

- clamp(0.25 × MemAvailable, 512 MiB, 8 GiB);
- capped at 0.5 × (memory.max − memory.current) when cgroup v2 sets a
  limit (Flatpak, containers);
- pure, and tested with text fixtures;
- rechecked every 5 s on the upload thread. Below 1 GiB available, it
  shrinks and evicts history.

It is configurable through Settings, `--cache-mb` and
`SPHERECUT_FRAME_CACHE_MIB`. On the iGPU machines, MemAvailable already
reflects GPU use, because the GPU shares the RAM.

| Machine | RAM | Budget | 5.7K 8-bit frames |
|---|---|---|---|
| huginn | 128 GB | 8 GiB (the cap) | 345 (11.5 s) |
| odin | 32 GB if that is right (the report will say) | about 6 GiB | about 258 |

**Contents.** `Arc<VideoFrame>` keyed by (stream, pts) in a `BTreeMap`,
with byte accounting from the frames' real `AVBufferRef` sizes. Runs link
frames by (run, seq), so the frame after a given one is known without a
decode, which makes VFR step and step back instant.

**Policy:**

- **ahead** holds frames with pts above the playhead, up to min(ahead ×
  rate × frame_bytes, budget / 2);
- **behind** takes the rest;
- eviction takes the frame farthest behind the playhead first, and frames
  ahead are evicted only after a seek moves the playhead past them.

**Back-pressure.** The decode worker waits on the cache condvar while the
ahead bytes are at the limit, and the presenter's playhead updates notify
it. A budget below 6 frames of the current source raises the "frame cache
too small" notice and still plays.

**Catch-up.** While playing, if the newest decoded frame is more than two
frames behind the clock, the worker calls `catch_up(Some(clock))`. That
skips non-reference frames and never downloads late ones, which count as
decode-dropped. Once caught up, `catch_up(None)`.

**GPU slots** (`SlotRing`, in an `Arc` shared by the upload thread and the
UI):

```
 Free ──(upload claims)──► Writing ──(submit returned)──► Ready{pts, run} ──(present picks)──► Shown
   ▲                                                         │ stale run after a seek                │ replaced
   └──────────── Released and Arc strong_count == 1 ◄────────┴──────────────────────────────────────┘
```

- Each slot has an `AtomicU32` state, an `AtomicI64` pts, an `AtomicU64`
  run, and a `Mutex<Option<Arc<GpuFrame>>>` that the UI only `try_lock`s.
  On contention the UI shows the current frame one refresh longer.
- **What gets uploaded:**
  - while playing, frames in pts order from the shown one forward;
  - when paused, the target first, then its successor and predecessor, so
    a step is instant.
- After a seek, slots of a stale run go back to Free unless one is shown.
  The shown frame stays up until the new one is Ready, so there is no
  black flash.

### 8.4 Seeking while playing

1. The UI sends `Seek { at, kind }` with a new generation from
   `StopSource::next()`. Earlier seeks stop at their next packet or frame.
2. The presenter sets the playhead to `at` and the state to Seeking, and
   keeps showing the current slot.
3. The audio epoch is bumped (section 9.2), and `audio-decode` runs
   `AudioDecode::seek(at)`.
4. The video worker checks the cache. If a run holds the frame for `at`,
   it uses it and continues from the end of the run. Otherwise it calls
   `seek` and inserts the result as a new run.
5. The upload thread uploads that frame first.
6. Preroll: playback resumes once a Ready slot holds the target and the
   ring holds at least 100 ms of the new epoch's audio. After 2 s it
   resumes on `SystemClock` with a notice.

A seek storm (1000 scrub events) costs at most one frame decode per
superseded request.

### 8.5 Shutdown

1. `begin_shutdown` sets the shutdown flag and calls
   `StopSource::shutdown()`. FFmpeg's interrupt callback ends blocking
   reads, and gated fakes return.
2. Every condvar is notified, and `audio-out` drops the cpal stream.
3. The upload thread finishes its current submit and polls with
   `PollType::Wait { timeout: 1 s }` on its own thread.
4. `join` waits up to 2 s per thread, polling `JoinHandle::is_finished`.
   It runs after `run_native` returns (in the GUI), or in the caller's
   thread (headless). `Drop for Player` calls `begin_shutdown` too.

### 8.6 Proving the UI never blocks

- **Static:** `crates/app/clippy.toml` (section 7.9).
- **Runtime:** `core::blocking::mark_ui_thread()` is called in the eframe
  creation callback and in each test harness. `may_block(what)` panics on
  the marked thread in debug and test builds, and logs once in release.
  playback, media and render call it from every blocking helper: cache
  lock and wait, channel `recv`, join, `Device::poll(Wait)`, readback and
  file I/O.
- **Negative control:** a test-only `Player::snapshot_blocking()` that
  takes the snapshot lock with `lock()` must panic when called from a
  marked thread. That proves the guard is live.
- **Engine test** (`crates/playback/tests/never_blocks.rs`), on
  `GatedBackend` gated forever:
  - a fake UI loop runs at 60 Hz for 3 s on a marked thread: `snapshot`,
    `present`, a seek every fifth iteration, play and pause toggles;
  - every iteration takes under 2 ms, and the test waits with
    `recv_timeout(10 s)`, so a hang fails instead of hanging;
  - the state reads Opening or Seeking;
  - after `gate.release()`, the first frame is presented within 500 ms.
- **Seek storm:** with a 50 ms-per-frame decoder, 1000 seeks in 2 s keep
  iterations under 2 ms, the final frame is the last seek's target, and
  fewer than 100 seeks run to completion.
- **Shutdown while gated:** `join` returns within 2 s.
- **GUI test** (kittest on lavapipe, `crates/app/tests/never_blocks.rs`),
  with the whole app on `GatedBackend` gated forever and the harness
  thread marked:
  - 60 `harness.step()` calls each return within 250 ms. That bound is
    lavapipe rendering, and the test catches hangs and long waits;
  - the viewer shows the "Decoding..." placeholder;
  - after release, the first frame appears within 30 steps.

## 9. Audio and A/V sync

### 9.1 Decode (`media::audio`)

- **Its own confined demuxer and decoder.** The decoder opens by its int
  codec id. Sample format and `ch_layout` (order, `nb_channels`, mask) are
  read as ints (D11).
- **Resampling** goes through an RAII `Resampler` in `ffi::resample`
  (`swr_alloc_set_opts2`, `swr_init`, `swr_convert_frame`), to packed f32
  at the device rate. ffmpeg-next's resampler takes bindgen enums, and the
  bitflags `ChannelLayout` before FFmpeg 7 (D11).
- **Channels:**
  - mono is duplicated, and stereo passes through;
  - 5.1 and up use swr's default downmix;
  - first-order ambiX (SA3D, the 4.0 layout of D17) is kept as 4
    unspecified-order channels, so swr can't remix it as surround. It is
    decoded to stereo in Rust: L = ½(W + Y), R = ½(W − Y), two virtual
    cardioids at ±90° (ACN/SN3D). The Info pane says "ambisonic: basic
    stereo decode (M6 adds binaural, steered by the view)".
- **Exact timing:**
  - libavcodec removes AAC priming (seekprobe: the first packet has pts
    −1024 and `skip_samples` 1024, and the first decoded frame has pts
    0), and the demuxer applies edit lists;
  - output sample s is clip time s / rate. Samples before 0 are dropped,
    and audio that starts after 0 gets leading silence;
  - a seek goes BACKWARD to at − 100 ms (at least two AAC frames of
    preroll for the MDCT overlap), flushes, decodes and trims to the exact
    sample. Samples after a seek equal those of a sequential decode within
    1e-6.

### 9.2 Output and the clock (`spherecut-audio`)

- **cpal 0.18.2** with `default-features = false` and `features =
  ["pulseaudio"]`.
  - The PulseAudio protocol host goes first. It is a pure-Rust client and
    talks to `pipewire-pulse` on Fedora and Arch.
  - ALSA is always compiled on Linux, as the fallback. Its `alsa-sys`
    links libasound, so building needs the ALSA headers and pkg-config.
    That is ALSA dev 1.2.11 here, `alsa-lib-devel` on Fedora and
    `alsa-lib` on Arch.
  - Streams start with `play()`.
- **Buffer size:** `BufferSize::Fixed(1024)` (21.3 ms at 48 kHz) when the
  supported range allows it. Otherwise the value nearest 1024 within the
  range, and `Default` only when the range is unknown. ALSA's default
  buffer can be very large. The actual period is measured from the
  callback and shown in the overlay.
- **The device's default rate and channel count**, with f32 preferred.
- **The chunk ring** is SPSC with fixed chunk slots of 512 frames each,
  0.5 s in all, in safe Rust:
  - samples are `AtomicU32` holding f32 bits;
  - each chunk header has atomic epoch, start sample and frame count;
  - head and tail use Release and Acquire.
  - **Flushing without a lock:** a seek bumps an `AtomicU64` epoch. The
    callback drops stale chunks and outputs silence until a chunk of the
    current epoch arrives.
  - The decode worker sleeps 5 ms while the ring is full, so the real-time
    callback never signals a condvar.
- **`AudioClock`:**
  - In the callback, it notes `Instant::now()` and cpal's
    `OutputStreamTimestamp { callback, playback }`, with latency =
    playback − callback.
  - It publishes {media sample, wall time + latency, valid until} through
    a seqlock of `AtomicU64`s.
  - `time_at(t)` = media + (t − wall), clamped to [media, valid until +
    one buffer], so a stalled callback freezes the clock instead of
    running ahead.
- **Underrun debt.** When the ring runs dry while playing, the callback
  outputs silence and the clock keeps running at the device's rate. The
  number of samples owed is recorded, and when data arrives the callback
  skips that many, so the next sample heard is at the right media time.
  Video keeps moving and A/V stays aligned. The overlay counts underruns.
- **Pause** freezes the clock and outputs silence, then `audio-out` pauses
  the stream.
- **No audio stream**, or one that can't be decoded: `SystemClock`, with a
  "no audio" notice.
- **No output device** (this box: cpal reports "requested audio device is
  not available"): `NullOutput` consumes the ring at the real-time rate
  from a thread and publishes the same clock, so the engine runs the same
  code. The notice says "No audio output device: playing silently".
- **Device lost** (a stream error or `DeviceChanged`), or **callbacks
  stalled over 500 ms** (a suspended sink): the player switches to
  `SystemClock` from the current media time and keeps playing. `audio-out`
  retries the default device every 2 s.

### 9.3 A/V sync

- **Video follows audio.** Each UI frame, the presenter shows the newest
  Ready slot with pts ≤ clock.time_at(now + present_latency) +
  av_offset.
- **present_latency** is 1.5 × the measured median UI frame interval, for
  egui-wgpu's frame latency of 2.
- **av_offset** is a setting in ms, default 0.
- **There is no drift to correct.** The DAC's clock is the master, and
  video frames repeat (30 fps on 60 Hz) or drop to follow it.

**Tier 1 A/V tests** (`crates/playback/tests/av_sync.rs`) use
`FakeAudioDevice`. It runs the real callback code with scripted
timestamps: a fixed period, a stated latency, and optional stalls and
underruns. Each combination of 29.97 or 59.94 fps with 44.1 or 48 kHz runs
60 s of simulated time:

- at every present, the shown frame is the one whose interval contains the
  audio clock's time (boundary arithmetic: frame k is chosen from the
  first present at or after pts_k);
- a seek restarts audio at exactly sample round(at × rate);
- a 200 ms decode stall leaves the clock running, video continuous, and
  the sample heard after recovery equal to the clock (the debt skipped);
- device loss switches to `SystemClock` without a jump over 1 ms;
- `NullOutput` keeps time within ±5% over 2 s.

**Tier 2** `audio_output_device_plays`:

- plays 2 s of tone on the default device;
- callbacks arrive within 2× the buffer period;
- the clock advances 2.0 s ± 50 ms against `Instant` and is monotonic;
- the reported latency is 5 to 200 ms;
- the host name, buffer and latency go into the report.

**By eye and ear:** the `sync-flash-beep` clip (a white flash frame and a
1 kHz beep on every whole second) on huginn and odin. The overlay shows
the A/V offset.

## 10. Performance overlay

F3 or View ▸ Overlay toggles an `egui::Area` over the viewer's top-left
corner. Values come from `PlayerSnapshot.perf` (a `try_lock` copy) every UI
frame.

```
VA-API · AMD Radeon 8060S (RADV STRIX_HALO) · HEVC Main 5760×2880 nv12 full/709 · 29.97 fps
decode    6.1 ms p95 9.8   download 2.3   decoded 61.2 fps (2.04× needed)   catch-up off
upload    CPU 1.4 ms   GPU 0.6 ms (ingest 0.0, mips 0.2)   write_texture
render    CPU 0.2 ms   GPU 0.5 ms   2560×1440 @1.25   aniso   projection skipped 112/144
UI        16.7 ms p99 17.9   hitches 0
video     shown 1798   dropped 2 (0.11%)   decode-dropped 0   A/V +4 ms (p95 9)
cache     1.21 / 8.00 GiB   ahead 31 (1.03 s)   behind 17   slots 2/4 ready
audio     pulse 48 kHz 2 ch   period 21 ms   latency 38 ms   underruns 0
memory    RSS 2.3 GiB   GPU alloc 0.42 GiB   minflt 12/s
[sparkline: UI frame interval, last 240]   [sparkline: shown video frame interval]
```

| Metric | Source | How |
|---|---|---|
| decode ms | decode worker | `Instant` around send and receive per frame, without the download. With frame threading, per-frame time misleads, so the headline is decoded fps over the last second and its ratio to the stream rate |
| download ms | decode worker | around `download_into_pool` |
| upload CPU ms | upload thread | around the plane copies, recording and submit |
| upload GPU ms | GPU timestamps | around ingest and mips (`TIMESTAMP_QUERY_INSIDE_ENCODERS`) |
| render CPU ms | UI thread | around `prepare` |
| render GPU ms | GPU timestamps | `RenderPassTimestampWrites` on the projection pass |
| UI frame ms, hitches | UI thread | the interval between `logic` calls; a hitch is over 1.5× the median |
| dropped | presenter | when the shown frame goes from A to B, the frames between them in the run (from run links, else from pts gaps over the frame duration); not counted while no UI frames are painted |
| decode-dropped | decode worker | frames skipped by catch-up |
| A/V | presenter | clock(now + latency) − shown pts: median and p95 over the last second |
| cache, slots | FrameCache, SlotRing | bytes, budget, frames and seconds ahead and behind, slot states |
| audio | audio crate | host, rate, period, latency, underruns |
| memory | upload thread, once a second | `/proc/self/statm` and `/proc/self/stat` (minor faults); `device.generate_allocator_report()` |

- **GPU timestamps.** A ring of 4 `QuerySet`s with resolve and readback
  buffers.
  - Each readback is mapped with `map_async` a frame later, and read when
    its callback has fired, never waited for. The upload thread's
    `PollType::Poll` and eframe's submits drive the callbacks.
  - The period comes from `queue.get_timestamp_period()`.
  - Without `TIMESTAMP_QUERY` the overlay shows "n/a". On lavapipe the
    numbers are labelled CPU timings.
- **Recording.** Per-stage rings of 256 f32 samples sit in a `Mutex` that
  only their one producer locks. The UI `try_lock`s once a frame and
  shows the previous copy on contention. Counters are `AtomicU64`.
- **One source of numbers.** The same `PerfStats` feed the overlay, the
  `--smoke` JSON and the Tier 2 benchmarks, so the overlay, the hardware
  report and `docs/perf.md` agree.

## 11. Dependencies

Versions are as locked in the stack probe (`scratchpad/m1/stackprobe`).
All come from crates.io, so deny's sources check passes. wgpu and the egui
family are pinned with `=`, because they move in lockstep and minor
releases break APIs (D7). Every row becomes a D22 row.

| Crate | Version and features | Used by | Why | License |
|---|---|---|---|---|
| wgpu | `=30.0.1`, `default-features = false`, `["std", "parking_lot", "vulkan", "wgsl"]` | render, playback, app, cli | D7: the Vulkan-only GPU API that egui-wgpu 0.37 needs. Brings wgpu-core, wgpu-hal, wgpu-types and naga 30.0.1, ash 0.38, gpu-allocator 0.28, gpu-descriptor and parking_lot | MIT OR Apache-2.0 |
| bytemuck | `1.25.2`, `["derive"]` | render | Pod uniform structs with no unsafe of ours (compiles under `forbid(unsafe_code)`). Already in wgpu's and epaint's trees | Zlib OR Apache-2.0 OR MIT |
| pollster | `1.0.1` | render, cli | `block_on` for adapter and device requests and readback, off the UI thread. eframe needs it anyway | Apache-2.0 OR MIT |
| image | `0.25.10`, `default-features = false`, `["png"]` | render, cli, app | 8- and 16-bit PNG writing, golden reading, smoke screenshots. eframe already requires exactly this version, and kittest's `render()` returns `image::RgbaImage` | MIT OR Apache-2.0 |
| eframe | `=0.37.0`, `default-features = false`, `["wgpu_no_default_features", "wayland", "x11", "default_fonts", "persistence", "clipboard"]` | app | D7. `clipboard` gives the doctor banner's copy buttons and passed deny and audit in simplicity's probe of the same graph. Brings egui-winit, winit, smithay-client-toolkit, the wayland crates (dlopened), x11-dl, ron and arboard/smithay-clipboard | MIT OR Apache-2.0, plus the fonts (section 16, Q1) |
| egui | `=0.37.0` | app | D7 | MIT OR Apache-2.0 |
| egui-wgpu | `=0.37.0`, `default-features = false` | app | `CallbackTrait`, `RenderState`, `WgpuSetupCreateNew` | MIT OR Apache-2.0 |
| egui_tiles | `=0.18.0` (default `serde`) | app | D7 docking | MIT OR Apache-2.0 |
| winit | `0.30.13`, `default-features = false`, `["rwh_06", "wayland", "wayland-dlopen", "x11"]` | app | only to turn `wayland-dlopen` back on, since eframe without defaults drops it and the build has no libwayland dev; `wayland-csd-adwaita` stays off | Apache-2.0 |
| cpal | `=0.18.2`, `default-features = false`, `["pulseaudio"]` | audio | audio output with stream timestamps. Brings alsa and alsa-sys (links libasound), pulseaudio 0.3, futures, dasp_sample, portable-atomic, and a second thiserror (1.x) | Apache-2.0; pulseaudio MIT; alsa Apache-2.0/MIT |
| libc | `0.2.191` (already in the tree) | cli | `sigaction` and `SA_RESETHAND` in `cli::interrupt` | MIT OR Apache-2.0 |
| egui_kittest (dev) | `=0.37.0`, `["wgpu"]` | app | GUI tests on lavapipe, through `WgpuTestRenderer::from_render_state`. Not `eframe` (it turns on `eframe/accesskit`) and not `snapshot` (we compare images ourselves). In test builds it turns on wgpu's `gles`, `metal` and `dx12` features; the shipped binary has none of them, and our instance selects Vulkan only | MIT OR Apache-2.0 |
| naga (dev) | `=30.0.1`, `["wgsl-in"]` (already in the tree) | render | the uniform layout test (section 4.5) | MIT OR Apache-2.0 |

serde_json becomes a dev-dependency of render (the golden manifest) and a
normal one of app (the smoke JSON). It is already in the tree.

**Not added:**

- `rfd` (zbus advisories, or GTK 3);
- `arc-swap`, `crossbeam`, `rtrb` and `ringbuf` (std atomics suffice);
- `parking_lot` as a direct dependency;
- `egui_plot` (sparklines are 20 lines of painter code);
- `criterion` (D20's bench harness covers M1);
- `signal-hook` and `ctrlc` (libc is already here);
- `image-compare` and `dssim` (rayon; AGPL-3.0);
- eframe's `accesskit`, `links`, `system_fonts` and `glow`;
- winit's `wayland-csd-adwaita`;
- cpal's `pipewire` and `jack`.

The Linux normal tree grows from 58 crates (`cargo tree -e normal
--workspace` today) to about 220. That is the cost of D7's stack.

**`deny.toml` changes:**

1. **Licenses** (needs Pat's answer, section 16 Q1). Recommended: under
   `[licenses] exceptions`, add `{ crate = "epaint_default_fonts", allow =
   ["OFL-1.1", "Ubuntu-font-1.0"] }`, scoped to that crate. Its license is
   `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`, and it is the only
   license failure in the probed graph.
2. **Feature bans** that keep the advisory-free choices from silently
   coming back (D23's `[[bans.features]]` pattern):
   - `{ crate = "eframe", deny = ["accesskit", "links"] }`: accesskit
     brings zbus 5 and RUSTSEC-2026-0370, -0371 and -0372; links opens a
     browser;
   - `{ crate = "winit", deny = ["wayland-csd-adwaita"] }`:
     RUSTSEC-2026-0192 through ttf-parser;
   - `{ crate = "egui_kittest", deny = ["eframe"] }`;
   - `{ crate = "cpal", deny = ["pipewire", "jack"] }`: build-time native
     dependencies, which this box lacks.
3. **No advisory ignores.** The probe runs show advisories, bans and
   sources passing without them.
4. `multiple-versions` stays a warning (about 23 duplicates: bitflags,
   thiserror 1 and 2, syn, hashbrown, rustix). `[graph] all-features =
   true` covers only our own features, of which only `media/fault-injection`
   is optional, and that one adds no dependency.

**Other files:**

- the root `clippy.toml` gains the `std::net` `disallowed-types`;
- `crates/app/clippy.toml` (section 7.9);
- `.gitignore` adds `/target/golden-diffs/`;
- the README gains a "Runtime libraries" note: libasound, libwayland-client
  and libxkbcommon, loaded at run time.

## 12. Tests

### 12.1 Tier 1 (here: 4 CPUs, lavapipe, FFmpeg 6.1.1, no GPU, no audio device)

**New generated clips** (synth, made once per run into
`CARGO_TARGET_TMPDIR`):

- open-GOP HEVC (x265 `open-gop`, keyint 8, B-frames 3) and open-GOP H.264
  (x264 `open_gop=1`, keyint 8, B-frames 3);
- a lead-in edit list (`Mp4Writer` gains an `edit_start` option that
  writes an `elst`);
- true VFR at 1/90000 (`Mp4Writer::push_video_at`);
- a Matroska remux at 1/1000 (`synth::remux_to_mkv`, FFmpeg's Matroska
  muxer through our own `StreamIo`);
- 29.97 and 59.94 fps clips;
- full-range H.264, BT.601-tagged H.264, lossless 10-bit HEVC, 4:2:2
  10-bit, and 4:4:4 at 8 and 10 bits;
- `sync-flash-beep`;
- `long-gop` (1920×960 lossless HEVC, one 600-frame GOP) for the SIGINT
  test.

Synth's `encode::VideoSettings` replaces `bt709_limited` with
`ColorTags { range, matrix, transfer, primaries, chroma_location }`.

| Crate | Tests |
|---|---|
| core | `TimeSpec` grammar (proptest: decimals exact, h:m:s, rationals, `Nf`, refusals); `Time`, `Rational` and `FrameTime` serde in JSON and ron; the colour default table; `StopSource` generations; `may_block` panics on a marked thread (and not elsewhere); `TempFile::persist_new` refuses an existing output, loses a race to a file created between the check and the publish with EEXIST, and takes the copy fallback when hard links are refused (test hook); `persist_replace` keeps D16's tests |
| sphere | `orient` axis checks (yaw 90 maps −Z to +X, pitch 90 to +Y, roll's sign) and a round-trip proptest; `pixel` round trip; the `grab` proptests (section 7.4); `project_morph` round trip; `uv_gradient` against finite differences, across the seam and near the poles; `zoom_about` keeps the cursor direction |
| media | `pixel_layout` for every native format, and swscale for the rest; a zero-copy `plane()` (pointer equal to `AVFrame.data`); colour fields from the tagged clips; Send and Sync assertions; the render node lookup on a fake sysfs tree (bus id hit, vendor and device scan, fallback) |
| synth/tests (media + synth) | **seek oracle**, `seek.rs`. For every clip above, at every frame's pts, a random time inside each interval, and the boundary targets (interval start, middle, next pts − 1 tick): `seek` returns exactly the sequential-decode frame (pts and FNV hash of the planes), and the next 3 `next_frame` calls match. Before the start gives the first frame, past the end gives `AfterLast`, and a truncated clip gives `Truncated` and seeks before its cut. `Keyframe` returns a key frame at or before t, then continues to t. A `StopToken` stops a long seek. `--frame N` uses (N + ½) on the MKV remux. **Negative control:** a test-only naive BACKWARD seek must miss frames on both open-GOP clips (seekprobe: 11/40 and 8/40). **Fallback:** FFmpeg's Vulkan device on lavapipe gives `FellBack(Refused(NotOffered))` with one event; `Fallback::Fail` gives `HwUnsupported`; a missing render node gives `FellBack(DeviceUnavailable)`; `fault-injection` at frame k continues bit-exact against software with exactly one event. **Audio:** sample counts; 440 and 660 Hz by zero crossings; a seek matches sequential decode within 1e-6; 44.1 to 48 kHz length exact over 10 s; AAC priming starts on the video axis; ambiX at azimuth +90° gives an L/R energy ratio over 4; in `sync-flash-beep`, each beep onset is within 1 ms of its flash frame's pts |
| render (lavapipe) | the goldens of section 6; `mips_area`; every pipeline builds; the naga layout test; readback at odd sizes (row padding); `TooLarge`; the upload bench's smoke run |
| testkit | `psnr`, `ssim` and `diff_stats` on known values (identical images give ∞ and 1.0; Gaussian noise gives the expected PSNR within 0.1 dB); `fnv64` test vectors |
| audio | the chunk ring under a two-thread stress of 10⁷ samples (order kept, nothing lost except across flushes); epoch flushes; the seqlock clock is monotonic and clamped; underrun debt arithmetic; the buffer size choice from Fixed, Range and Unknown |
| playback | cache proptests (never over budget; ahead never evicted inside the window; the producer blocks at the limit and resumes; run links survive evictions); `auto_budget` fixtures, cgroup included; the presenter rule with VFR; drop counting; a slot is reused only once its `Arc` count is 1; `never_blocks`, the seek storm and shutdown (section 8.6); the fake-device A/V tests (section 9.3); a fallback event reaches the snapshot |
| app (kittest, lavapipe) | the synthetic source renders, with the red F marker at the centre; marker drag at ppp 1.0 and 1.25; zoom about the cursor; the projection morph's end state; step forward and back read the next and previous frame codes; ppp 1.0, 1.25, 1.5 and 2.0 byte-identical to `render_still`; `ui()` and `prepare` rounding agree; the resize sweep; GUI `never_blocks`; the doctor banner from a fixture report; overlay fields non-zero after playing on `ManualClock`; the fallback and no-audio notices |
| cli | `frame.rs`: the Euler table below, the frame code read back through a 360° fisheye, every error row with no output left behind, `--reference` against the GPU at the calibrated tolerance, and the pipeline goldens and swscale cross-check (section 6). `contract.rs`: envelopes for probe, doctor and frame, exit codes 0 to 4 (no adapter by running the child with `VK_DRIVER_FILES` pointing at no ICD; `hw_unavailable` with `--decoder vaapi --render-node /nonexistent`; SIGINT 300 ms into a seek on `long-gop`). `confinement.rs`: the table in section 3.6. `no_network.rs`: strace. `probe.rs`: the `lrc` test now expects the refusal |
| xtask | prerequisite detection (strace missing fails with the install command); hw-test's new environment parsing, against fixtures |

The Euler table (`frame` on the grid clip, centre pixel):

| Arguments | Centre pixel |
|---|---|
| none | red, F |
| `--yaw 90` | green, R |
| `--yaw 180` | blue, B |
| `--yaw -90` | yellow, L |
| `--pitch 90` | white, U |
| `--pitch -90` | black, D |
| `--roll 90` | the U marker on the right edge's side |

**The confinement table** (`crates/cli/tests/confinement.rs` and
`no_network.rs`). Every file is generated into a temp dir:

| Input | Expected |
|---|---|
| `dir/list.m3u8` naming `../outside/seg.ts` | `unsupported_container` naming "hls", and no open of `seg.ts` in the FFmpeg log or under strace |
| `ffconcat version 1.0` with `file 'clip.mp4'` | `unsupported_container` naming "concat" |
| a playlist with `http://127.0.0.1:1/seg.ts` | `unsupported_container` naming "hls" |
| the path `http://127.0.0.1:1/x.mp4` | `not_found`, "Spherecut opens local files only" |
| an MP4 opened with a 1000-pixel decoder limit (test hook) | `too_large` |
| an MP4 with a `dref` URL entry, opened with `enable_drefs=1` in a test-only open | `nested_open_refused` naming the URL: proves `io_open` is installed |
| an `.lrc` file | `unsupported_container` naming "lrc", exit 1 |
| `strace -f -qq -e trace=socket,connect -o LOG spherecut probe clip.mp4`, then `spherecut frame clip.mp4 --adapter cpu -o out.png` | no `AF_INET` or `AF_INET6` in LOG. strace is a prerequisite of `cargo xtask check`, so this test fails, never skips, without it |

**Runtime.** Slices 3 and 7 measure what M1 adds to `cargo xtask check`
on this box. The target is at most 3 minutes more on 4 CPUs, with clips
generated once per run and small golden sizes.

### 12.2 Tier 2 (`#[ignore = "tier2: …"]` plus `require_*`)

Each test calls its `require_*` gate. A skip is reported as TIER2-SKIP
with the reason (D19), and it never counts as a pass for a hardware
criterion.

| Test | Crate | What it records or asserts |
|---|---|---|
| `list_adapters_and_features` | render | every Vulkan adapter: `AdapterInfo` with `device_pci_bus_id`, features, limits, downlevel flags, `get_texture_format_features` for R8, Rg8, R16Uint, R16Float, Rg16Float, Rgba16Float, NV12 and P010, the timestamp period. Fails if a canonical format is not renderable and filterable |
| `goldens_on_gpu_match_reference` | render | RADV (or ANV) against the CPU and ideal references, isotropic |
| `goldens_on_host_llvmpipe` | render | the stored goldens on the machine's llvmpipe; records the drift |
| `bench_upload_5k7` | render | `write_texture` against the test-side belt for 8- and 10-bit 5.7K and an 8K pair; ingest, mips and projection GPU ms at 1920×1080 and 3840×2160, rect 90° and tiny planet 300°; allocator block counts over 300 frames |
| `vaapi_seek_matches_software` | synth/tests | 50 random seeks per stream on the noisy 5.7K clips and every X5 sample stream, with `Fallback::Fail`: VA-API equals software bit for bit, and the pts equal sequential decode |
| `vaapi_colorinfo_matches_software` | synth/tests | range, matrix, transfer and siting resolved identically from both paths |
| `vaapi_download_pool_reuses_buffers` | media | under 50 minor faults per frame after 30 warm-up frames |
| `vaapi_render_matches_software` | cli | the same RGB within 1 LSB |
| `bench_seek_latency` | synth/tests | median and p95 from `seek()` to frame, exact and keyframe, on the noisy 5.7K clip, the X5 8K and the LRV |
| `bench_playback_realtime_5k7_8bit`, `_10bit` | playback | the headless engine with VA-API, a real-time `SystemClock`, an offscreen 1920×1080 present at a simulated 60 Hz, the 20 s noisy clip looped for 60 s (a seek every loop). Records per-second decoded fps (minimum and median), per-stage p50 and p95, and drops. **Passes only with 0 drops after the first second**, and an unthrottled per-second minimum of at least 1.2 × 29.97 |
| `bench_playback_x5_raw` | playback | the X5's 8K dual stream decoded and uploaded as two slots in the raw view. This is M3's 8K budget, measured now; it records and does not gate |
| `bench_playback_stitched_samples` | playback | each stitched equirect in `samples/` (clip 12 when it arrives); skips with the reason otherwise |
| `audio_output_device_plays` | audio | section 9.3 |
| `gui_smoke` | app | section 12.4 |

### 12.3 The M1 hardware runs

Requests are batched, and Tier 1 work continues while waiting:

- **H1, after slice 5:** huginn and odin, from any terminal. odin has never
  run hw-test, so its first report also covers M0's checks there. H1
  covers:
  - Tier 1 on the machine;
  - the adapter dump;
  - goldens on RADV and on the local llvmpipe;
  - the upload bench (which decides the belt trigger);
  - seek parity and latency;
  - the real-time playback benches;
  - the X5 raw bench.
- **H2, after slice 8:** huginn and odin from a terminal inside GNOME, at
  100% and at a fractional scale (125% or 150%), plus Hyprland if a
  machine has it (section 16, Q6). H2 covers:
  - the GUI smoke run with screenshots;
  - the audio device test;
  - the checklist by eye.
- **H3, in slice 9:** both machines on the final commit.

**Pass criteria for "5.7K30 stitched equirect plays in real time"**, on
both machines with VA-API, for the noisy 5.7K 8-bit and 10-bit clips, and
for clip 12 once Pat supplies it:

- `bench_playback_realtime_5k7` drops 0 frames after the first second over
  60 s;
- the per-second decoded fps never falls below 1.2 × 29.97 (36.0 fps);
- `gui_smoke` drops at most 0.1% of frames, with UI p99 under 1.5× the
  refresh period, and |A/V| p95 under 20 ms on the sync clip.

The numbers go into `docs/perf.md`. Until clip 12 arrives, PROGRESS says
the real-time criterion was met on synthetic content at camera bitrates,
and the criterion stays "done pending hardware" for real stitched footage.

**What Pat checks by eye:**

- smooth panning (drag) while playing, with the point staying under the
  cursor;
- no line when yawing slowly through 180° on the seam clip;
- no crawl in tiny planet and 360° fisheye on the high-frequency clip;
- the X5 LRV's colours against mpv (full range);
- the flash and the beep landing together on `sync-flash-beep`;
- GNOME's window decorations;
- sharp text and video at 125% and 150%.

**Screenshots:** the smoke run's own, plus one full-desktop screenshot
per desktop from Pat, showing the decorations.

### 12.4 What `cargo xtask hw-test` and `cargo xtask check` gain

1. **Prerequisites.** `check_prereqs` (`hw_test.rs:229`) adds:
   - the ALSA headers, through `pkg-config --exists alsa`: `sudo dnf
     install alsa-lib-devel` on Fedora, `sudo pacman -S alsa-lib` on Arch;
   - strace: the `strace` package on both;
   - rustc 1.95 or newer: Fedora's RPM is 1.99, otherwise rustup;
   - a lavapipe ICD for the Tier 1 render tests: `mesa-vulkan-drivers` on
     Fedora, `vulkan-swrast` on Arch.

   `cargo xtask check` checks strace and the ALSA headers itself, and fails
   with the install command.
2. **Tier 1 on this machine.** hw-test runs `cargo test --workspace
   --all-targets` and reports pass, fail and the failure output in a new
   "Tier 1 on this machine" section. This catches FFmpeg 8.1.3's seek
   behaviour and Mesa 26's llvmpipe drift.
3. **Environment** additions:
   - `XDG_SESSION_TYPE` and `XDG_CURRENT_DESKTOP` (as now), plus whether
     `WAYLAND_DISPLAY` was set or discovered;
   - `pactl info`'s server line;
   - `gsettings get org.gnome.mutter experimental-features`;
   - `hyprctl version` and `hyprctl monitors`;
   - the power profile, read-only: `/sys/firmware/acpi/platform_profile`,
     plus amdgpu's `power_dpm_force_performance_level` and
     `pp_power_profile_mode`. This is for the VA-API variance (huginn's X5
     8K30 ran at 1.17× to 2.41×);
   - every render node with its PCI slot, vendor, device and driver.
4. **Perf media.** hw-test generates
   `target/hw-media/hevc{8,10}-5k7-30fps-noisy-20s.mp4` with libx265 at
   about 100 Mbps (keyint 30, B-frames on), from synth's moving grid with
   seeded per-frame noise. The clips are cached across runs. They are not
   byte-reproducible and don't need to be. The report records their
   software decode fps and flags a clip "too easy" above 150 fps; the easy
   grid clip decoded at 330, and the X5's own 5.7K60 at about 58.
5. **The GUI smoke run** fills the "## GUI smoke run" section
   (`hw_test.rs:1316`) from the Tier 2 test `gui_smoke`.
   - **Finding the session:**
     - use `WAYLAND_DISPLAY` when set;
     - from a TTY (huginn's last report was "tty / ?"), use the single
       `wayland-*` socket owned by the user in `$XDG_RUNTIME_DIR`, for the
       child process only;
     - otherwise `DISPLAY`;
     - otherwise TIER2-SKIP: "no graphical session: run from a terminal
       inside GNOME or Hyprland, or leave the desktop unlocked and on
       screen".
   - **The runs:** it spawns `target/release/spherecut --smoke <clip>
     --smoke-seconds 20 --smoke-out <json> --smoke-screenshot <png>` for
     the noisy 8-bit and 10-bit clips, the seam clip, the sync clip, each
     stitched sample, and the X5 raw view.
   - **A hidden session can't pass.** If fewer than 90% of the expected UI
     frames were presented, the compositor did not draw (a locked session,
     or one on another VT), and the test fails.
   - **The JSON:**
     - adapter, surface format, scale factor and window size;
     - UI frame p50, p95, p99 and max;
     - shown, dropped and decode-dropped frames;
     - decode, download, upload and render p50 and p95;
     - A/V offset median and p95;
     - audio host and latency;
     - cache and slots.
   - **The screenshot** comes from `ctx.request_screenshot` (egui 0.37's
     callback API; there is no `Event::Screenshot`). It is downscaled to at
     most 960 px wide (about 0.5 to 1 MB) and saved as
     `hw-reports/<host>-<commit>-gui-<n>.png`, linked from the report.
   - **FAILED when** drops are over 0.1%, UI p99 is over 1.5× the refresh
     period, |A/V| p95 is at least 20 ms on the sync clip, or decode runs
     in software when doctor says VA-API HEVC works.
6. **Benchmarks** need nothing new: the `bench_` rows land in the report
   automatically (D19, D20), and `docs/perf.md` gains rows for playback,
   upload, seek latency and the X5 raw view.

## 13. Risks, ranked, and how each is retired early

| # | Risk | Retired by | When |
|---|---|---|---|
| 1 | **5.7K30 misses real time** once download, upload, ingest, mips and projection join VA-API decode. huginn decodes synthetic 5.7K at 120.8 fps including the copy to RAM, but its X5 8K30 runs at 1.17× to 2.41× | The headless real-time benches go to huginn and odin in H1, before any GUI, with per-second fps and the power profile recorded. Fallbacks, in order: catch-up mode; generating only the mip levels the view needs (the CPU knows the coarsest footprint); the belt and then U2; pulling M9's DMA-BUF spike forward | slice 5, H1 |
| 2 | **No real stitched 5.7K sample** (clip 12 is still wanted), and synthetic content is easy (330 fps in software) | Ask Pat in slice 1. Meanwhile the noisy 100 Mbps clip with its "too easy" check, and `bench_playback_x5_raw` (two 3840² streams, heavier than one 5.7K stream) | slice 1, H1 |
| 3 | **odin has never run hw-test**: its toolchain, RPM Fusion setup, VA-API on RDNA3 and a possible iGPU are all unknown | Request odin's M0-level hw-test in slice 1, on current main | slice 1 |
| 4 | **The fonts license question blocks the GUI** (a license-affecting dependency is ask-first) | Ask in slice 1, six slices before it is needed. If still open at slice 7, slice 7 waits on a branch, built and tested but not merged, while slice 8's non-GUI parts proceed | slice 1, needed by slice 7 |
| 5 | **Seek on real camera files and FFmpeg 8.1.3** behaves differently from FFmpeg 6.1.1 here | The oracle with its negative control in slice 2; Tier 1 on the hardware machines in H1; VA-API parity on every X5 stream | slice 2, H1 |
| 6 | **Upload cost**: `write_texture`'s per-call staging and block churn for 8K pairs, and row-by-row copies for unaligned software strides | Pooled 256-aligned downloads from slice 2; `bench_upload_5k7` with the belt comparison and allocator report, with a stated trigger | slice 3, H1 |
| 7 | **A/V sync through the pulse protocol host on PipeWire** (latency reporting quality, device changes) | The fake-device tests at 29.97 and 59.94 × 44.1 and 48 kHz; the Tier 2 device test; the flash-and-beep clip by eye; the A/V offset setting; ALSA as a second host; the switch to `SystemClock` on loss or stall | slice 6, H2 |
| 8 | **Wayland behaviour**: GNOME's plain decorations, fractional scaling, no touchpad pinch, no file drop, and a smoke run started from a TTY | kittest at four scales and a resize sweep in slice 7; the gaps documented with fallbacks (scroll zoom, the Files pane); session discovery plus the presented-frames check; screenshots in H2 | slices 7 and 8, H2 |
| 9 | **No Hyprland machine** until the Framework arrives | Ask whether huginn or odin can log into Hyprland (Fedora packages it). Otherwise that bullet stays "done pending hardware" | slice 1, H2 |
| 10 | **Golden drift across Mesa and LLVM** (25.2.8 and LLVM 20 here, 26.2.5 and LLVM 22 on huginn) | Exact match only at the same driver build, tolerances otherwise; source hashes; `goldens_on_host_llvmpipe` and Tier 1 on huginn in H1 | slice 3, H1 |
| 11 | **GPU memory**: 10-bit slots with f16 canonical planes on the RX 7600's 8 GB, and UMA pressure on huginn and the Framework | 4 slots by default (265 MB at 10-bit 5.7K), allocation inside an OOM error scope that drops a slot, the cgroup-aware cache budget, and a Tier 1 test playing 5.7K synthetic planes with a 256 MiB cache | slice 5 |
| 12 | **Flaky timing tests** on a loaded 4-CPU box (`never_blocks`, kittest steps) | Generous margins (2 ms against 50 ms gates, 250 ms per lavapipe step), budgets measured and recorded in slices 5 and 7, and the GUI test in its own test binary | slices 5 and 7 |
| 13 | **eframe 0.37 API surprises** (the release is days old) | Signatures checked against the registry sources: `App::ui` and `logic`, `CallbackTrait`, `ScreenDescriptor`, `ViewportInPixels::from_points`, `request_screenshot`, `WgpuTestRenderer::from_render_state`, `HarnessBuilder::with_pixels_per_point`. Slice 7 starts with a 50-line spike of the callback in kittest | slice 7 |

## 14. Slices

Each slice lands on `main` working, with fmt, clippy `-D warnings`, Tier 1,
deny and audit green, and with its DECISIONS and PROGRESS updates. Then it
is **pushed**: Pat's machines clone from GitHub, so a hardware request
needs a pushed commit.

### Slice 1: FFmpeg confinement and the JSON contract for probe and doctor

- **Lands:**
  - `ffi::confine` (custom AVIO, the `io_open` refusal, the demuxer
    allow-list, `protocol_whitelist`, `format_whitelist`, `max_pixels`),
    used by probe, the decoders and doctor;
  - the root `clippy.toml` `std::net` ban;
  - `MediaError`'s new variants and `code()`;
  - `Time`, `Rational` and `FrameTime` serde;
  - probe's times as `Time`;
  - `cli::contract`, with probe and doctor on the envelope and exit codes
    0, 1 and 2;
  - probe refusing other containers by name, with the `lrc` test, the
    README and D15 updated;
  - `docs/cli-json.md`;
  - strace as a `cargo xtask check` prerequisite;
  - D24 and D25.
- **Also:** sends Pat the questions in section 16 and the odin hw-test
  request.
- **Acceptance:**
  - the confinement table passes, including the strace test;
  - `contract.rs` passes for probe and doctor (one compact document per
    run, the same shape for one file or many, exit codes 0, 1 and 2,
    doctor's errors exiting 1);
  - `cargo deny check` and `cargo audit` are green, and the commit is
    pushed.

### Slice 2: media backend, frames and frame-exact seek

- **Lands:**
  - `core::video` and `core::cancel`;
  - `media::backend` with `FfmpegBackend`, `VideoDecode` and the seek
    algorithm (section 3.3);
  - `catch_up`;
  - native pixel layouts, plus swscale to 4:4:4 for the rest;
  - the colour default rules with `assumed` flags;
  - the pooled download;
  - `media::hw` render node lookup;
  - fallback at open and mid-stream, with the `fault-injection` feature;
  - synth's `ColorTags`, the new clips, `SyntheticBackend`,
    `GatedBackend` and `FaultBackend`;
  - Send and Sync assertions;
  - D27 and D28.
- **Acceptance:**
  - the seek oracle is exact on every clip and its negative control
    misses;
  - the fallback tests pass, including bit-exact mid-stream continuation;
  - the colour table tests pass;
  - Tier 2 `vaapi_seek_matches_software`,
    `vaapi_colorinfo_matches_software`,
    `vaapi_download_pool_reuses_buffers` and `bench_seek_latency` are
    written and listed by `cargo test -- --ignored --list`.

### Slice 3: render core and goldens (library only), MSRV 1.95

- **Lands:**
  - workspace `rust-version = "1.95"`, with the D1 update;
  - wgpu, bytemuck, pollster and image, with their D22 rows;
  - sphere's `pixel`, `orient`, `uv_gradient` and `project_morph`;
  - `spherecut-render`: `Gpu`, the canonical planes, `Uploader`
    (`write_texture`, ingest, mips), `project.wgsl` with its filter and
    debug overrides, the encode pass, `render_still` and `reference`;
  - testkit's `compare` and `fnv64`;
  - the goldens with their manifest and the blessing gate;
  - the naga layout test;
  - D29 and D30.
- **Acceptance:**
  - every golden passes, and every negative control fails as designed
    (seam, poles and tiny planet under implicit LOD; shimmer at level 0;
    range, matrix and siting swaps);
  - `cargo +1.95 build --workspace --all-targets` and `cargo +1.95 test
    --workspace` pass;
  - Tier 2 `list_adapters_and_features`,
    `goldens_on_gpu_match_reference`, `goldens_on_host_llvmpipe` and
    `bench_upload_5k7` are written;
  - Tier 1 time measured and recorded.

### Slice 4: `spherecut frame` and the rest of the contract

- **Lands:**
  - `TimeSpec`;
  - `core::fs::TempFile` moved from inject, with `persist_new`;
  - the `frame` subcommand with every flag in section 5, the JSON result,
    `--overwrite`, and exit codes 3 and 4;
  - `cli::interrupt` (`sigaction` with `SA_RESETHAND`, a documented FFI
    module);
  - the pipeline goldens and the swscale cross-check;
  - the README's "How to run" for `frame`;
  - the D16 update.
- **Acceptance:**
  - `frame.rs` passes (the Euler table, frame code read back, every error
    row, `--reference`);
  - `contract.rs` passes for every exit code of section 5.1: 0 to 4 for
    `frame`, and still 0, 1 and 2 for probe and doctor, which never exit 3
    and keep the default SIGINT behaviour (only `frame` installs the
    handler);
  - strace shows no IP sockets for `frame`;
  - an existing output is never replaced without `--overwrite`;
  - Tier 2 `vaapi_render_matches_software` is written.

### Slice 5: the headless playback engine, then H1

- **Lands:**
  - `core::clock` and `core::blocking`;
  - `spherecut-playback`: `Player`, `FrameCache` and `auto_budget`, the
    decode workers with catch-up, the upload thread and `SlotRing`, the
    presenter, `SystemClock` and `ManualClock`, `PerfStats` and shutdown;
  - `may_block` calls in the blocking helpers;
  - hw-test's H1 additions (Tier 1 on the machine, prerequisites, power
    profile, render nodes, the noisy perf clips);
  - Tier 2 playback benches;
  - D32 and the D35 draft.
- **Acceptance:**
  - cache proptests, budget fixtures, `never_blocks` (engine part), the
    seek storm and shutdown pass;
  - a 256 MiB-budget 5.7K run plays without exceeding its budget;
  - **H1 is requested** with the list in section 12.3.
- **Afterwards:** the belt and U2 decisions are recorded from H1's numbers
  as soon as they come back.

### Slice 6: audio

- **Lands:**
  - `media::audio` (confined demuxer, `Resampler`, ambiX to stereo);
  - `spherecut-audio`: cpal output with pulse and ALSA, the chunk ring
    with epochs, `AudioClock`, underrun debt, `NullOutput` and
    `FakeAudioDevice`;
  - the engine's `audio-decode` and `audio-out` threads, audio as master,
    preroll, and device loss or a stall switching to `SystemClock`;
  - `sync-flash-beep`;
  - the ALSA prerequisite in `check` and hw-test;
  - the cpal D22 row;
  - D33.
- **Acceptance:**
  - the audio decode tests pass;
  - the fake-device A/V tests pass at all four rate combinations;
  - NullOutput keeps time on this box;
  - Tier 2 `audio_output_device_plays` is written.

### Slice 7: the GUI viewer

- **Lands:**
  - the font decision applied in `deny.toml`, after Pat's answer, plus the
    feature bans;
  - eframe, egui, egui-wgpu, egui_tiles, winit and egui_kittest, with
    their D22 rows;
  - `spherecut-app`: eframe setup sharing the device, the tiles, the
    viewer callback, grab and zoom in physical pixels, the projection
    morph, transport, the Files pane, notices and the startup doctor
    banner;
  - `media::doctor::render_text`;
  - `spherecut` with no subcommand opening the GUI, and `no_display` with
    exit 3;
  - `crates/app/clippy.toml`;
  - D31 and D34.
- **Acceptance:**
  - the whole kittest suite passes on lavapipe, including the
    byte-identical fractional-scale test, the marker drag at 1.25 and GUI
    `never_blocks`;
  - `cargo deny check` is green with only the font exception.

### Slice 8: overlay, settings, smoke, and H2

- **Lands:**
  - the overlay with GPU timestamps and sparklines;
  - cached ranges on the scrub bar;
  - persisted Settings (cache MiB, decoder, filter, slots, A/V offset,
    overlay);
  - the raw view for non-equirect sources;
  - `--smoke` with its JSON and screenshot;
  - `testkit::tier2::require_graphical_session` with TTY discovery;
  - Tier 2 `gui_smoke`;
  - hw-test's GUI section, environment lines and screenshots;
  - D35 final.
- **Acceptance:**
  - overlay and settings kittests pass;
  - the smoke driver passes under kittest (scripted, with no compositor);
  - here, `gui_smoke` reports TIER2-SKIP with the reason, never a pass;
  - **H2 is requested** from GNOME on huginn and odin, plus Hyprland if
    available.

### Slice 9: acceptance

- **Lands:**
  - fixes from H1 and H2, each with a Tier 1 reproduction when the bug
    isn't hardware-specific;
  - clip 12's numbers if it has arrived;
  - the `docs/perf.md` rows;
  - README "How to run" (GUI, keys, overlay, `frame`, runtime libraries);
  - PROGRESS;
  - ARCHITECTURE's Rendering and Threads sections, moved from planned to
    as built;
  - the PLAN wording amendment (D29);
  - every DECISIONS update in section 15;
  - a fresh-subagent review against PLAN M1, and its fixes;
  - H3 on the final commit;
  - the note to Pat.
- **Acceptance:**
  - every row of the table below has its evidence;
  - M1 is marked done, or "done pending hardware" for each hardware row
    still waiting (Hyprland, clip 12);
  - pushed.

### 14.1 PLAN M1 criterion, slice, evidence

| PLAN M1 criterion | Slices | Evidence |
|---|---|---|
| `MediaBackend` with FFmpeg: open, stream info, frame-accurate seek by PTS, decode to NV12/P010 (native layouts), software and VA-API with automatic fallback, audio decode | 2, 6 | the seek oracle and its control; the fallback tests; the audio tests; `vaapi_seek_matches_software` |
| FFmpeg opens only the file Spherecut opened, with the strace test, the clippy ban and probe's refusal by name | 1 | `confinement.rs`, `no_network.rs`, the `lrc` test, the README and D15 |
| Projection shader sampling the planes, per-source matrix and range, linear float target, sRGB output, equirect to rectilinear, fisheye and stereographic with morph | 3 | the goldens (projection, morph, range, matrix, siting, 10-bit); all pipelines build |
| No seam at ±180° and no shimmer: analytic LOD plus mipmaps, proven by goldens of the high-frequency pattern | 3 | `seam_quad`, `zenith_quad`, `nadir_quad`, `tinyplanet_pole` with the implicit-LOD control; `shimmer_*` with the level-0 control |
| `spherecut frame` | 4 | `frame.rs` |
| The JSON contract (probe, doctor, frame): one document, schema and version, `ok`, stable codes, the same shape for one or many, `{exact, seconds, frame}`, exit codes 0 to 4, `--overwrite` | 1, 4 | `contract.rs` |
| Goldens on lavapipe (each projection, seam, poles, 10-bit, limited and full) by PSNR/SSIM, and GPU matching the CPU reference | 3 | `goldens.rs`; `goldens_on_gpu_match_reference` |
| GUI: open a file, the viewer callback, grab (tested numerically), scroll and pinch zoom, projection switch, play/pause, scrub, frame step, audio playback with the audio clock driving A/V sync | 6, 7 | the kittest suite; the grab proptests; the fake-device A/V tests; H2 by eye |
| Decode, audio and upload on workers; the UI never blocks (slow-decoder harness); a bounded, configurable cache sized from memory | 5, 6, 7 | `never_blocks` (engine and GUI) with the `may_block` control; the cache proptests; `auto_budget` fixtures |
| Performance overlay: decode, upload and render ms, dropped frames, cache size | 8 | the overlay kittest; the smoke JSON |
| Startup codec and hardware check with the doctor's fixes | 7 | the doctor banner kittest; H2 |
| HiDPI and fractional scaling; GNOME and Hyprland, with a hardware report and screenshots | 7, 8 | the four-scale kittest; `gui_smoke` screenshots from H2 (Hyprland pending a machine) |
| 5.7K30 stitched equirect in real time on odin and huginn with VA-API; numbers in `docs/perf.md` | 5, 8, 9 | `bench_playback_realtime_5k7_*`, `gui_smoke`, `docs/perf.md` (clip 12 when it arrives) |

## 15. Decisions to record

D23 (agent integration) already exists, so M1's entries start at **D24**.
Each one lands in the slice that implements it.

**D24. FFmpeg opens only the file Spherecut opened** (slice 1; implements
D23's note; updates D8 and D15).
- **What:**
  - Spherecut opens the file itself and hands FFmpeg a custom AVIO
    (`StreamIo::from_read_seek`).
  - It replaces `io_open` with a refusal that records the URL through
    `AVFormatContext.opaque`.
  - It runs its own `av_probe_input_buffer2` check against the demuxer
    allow-list `["mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm"]`.
  - It sets `protocol_whitelist=file` and `format_whitelist` as defence in
    depth.
  - It gives every decoder `max_pixels = 16384 × 8192`.
  - Clippy bans `std::net`'s TCP and UDP types.
  - probe refuses other containers by name, and an strace test (with
    strace a `cargo xtask check` prerequisite) shows no IP socket.
- **Why:** `deny.toml` cannot see what the system FFmpeg does. M0's probe
  followed an HLS playlist to a file outside its folder, and an `http://`
  entry would have made FFmpeg use the network. When FFmpeg reads bytes
  only through our handle and every nested open is refused, it has no
  path to anything else. The allow-list keeps out the demuxers that
  reference other files (hls, concat, dash, image sequences). `max_pixels`
  bounds the memory a hostile header can ask for. Widening the list needs
  a new entry.

**D25. The CLI JSON contract, exit codes, `--overwrite` and SIGINT**
(slices 1 and 4; updates D16 and D18).
- **What:**
  - With `--json`, stdout carries one compact envelope per run: `schema`,
    `version` 1, `ok`, `result` or `error { code, message, path?,
    ffmpeg_log? }`, and `warnings`.
  - probe's result is always `files[]`.
  - Times are `{exact, seconds}`, plus `frame` when the time is a video
    frame on a CFR stream or was reached by counting, and rationals are
    `"n/d"`.
  - Exit codes: 0 ok; 1 an input or check failed, doctor's errors
    included; 2 usage, as an envelope when `--json` was given; 3 a codec or
    device the command itself needs is missing (for `frame`: no GPU, an
    unknown adapter, VA-API asked for but missing or refused, no decoder;
    for the GUI: no display); 4 SIGINT, through `sigaction` with
    `SA_RESETHAND`, leaving no output.
  - A command that writes refuses an existing output unless given
    `--overwrite`. It publishes with `hard_link`, which fails atomically
    with EEXIST, and falls back to `create_new` plus a copy where links are
    unsupported. `--overwrite` keeps D16's rename.
  - `docs/cli-json.md` lists the codes.
- **Why:** agents and scripts (D23, A1) need one parseable document and
  stable codes on failure as much as on success. One shape for one file
  or many saves every caller a special case. Exact times keep a frame's
  identity through JSON. The hard link closes the race between checking
  the output and renaming over it. `SA_RESETHAND` lets a second Ctrl+C
  kill a stuck run. Doctor reports capability, so its findings stay exit
  1, as D18 says.

**D26. Crate layout, rust-version 1.95 and where unsafe may appear**
(slice 3; updates D1).
- **What:**
  - New crates: `render` (wgpu; no media, no egui), `audio` (cpal),
    `playback` (workers, cache, slots, clocks; no egui) and `app` (eframe).
  - Frames reach render through `core::video::VideoPlanes`.
  - The workspace moves to rust-version 1.95.
  - `#![forbid(unsafe_code)]` everywhere except media, where `ffi` and
    `hwprobe` are allowed, and cli, where `interrupt` is allowed.
- **Why:** M2's export, `spherecut render` and M5 drive the engine with no
  window, and the goldens and M3's stitcher feed render without FFmpeg.
  `forbid` makes the unsafe rule mechanical, and bytemuck's derives
  compile under it. egui 0.37 needs 1.95. huginn's Fedora 44 RPM is 1.99,
  so the D1 note about Fedora's 1.95 is corrected.

**D27. MediaBackend and seek semantics** (slice 2; updates D13).
- **What:**
  - Clip time 0 is the first presented frame.
  - `seek(at, Exact)` returns the greatest pts ≤ origin + ⌊at/tb⌋, with
    ties going to the later frame, and it reports `Exact`, `BeforeFirst`,
    `AfterLast`, `KeyframeOnly` or `Truncated`.
  - The algorithm re-seeks to the first packet's dts − 1 until the first
    frame out is at or before the target (at most 8 attempts, then from
    the start).
  - Short forward targets decode forward, and nothing before the target is
    downloaded.
  - `Keyframe` mode serves scrubbing, and step back is pts − 1.
  - `--frame N` is (N + ½) × d for CFR, and counted for VFR.
  - Seeks stop through generation tokens. The AVIO interrupt is for
    shutdown only.
  - A seek clears truncation but not a hardware refusal.
- **Why:** seekprobe measured a naive BACKWARD seek missing 11/40 frames on
  open-GOP HEVC and 8/40 on H.264, because leading pictures after a
  keyframe whose pts is past the target are dropped silently. The
  re-seek missed none, edit lists and VFR included. The origin rule makes
  `--time 0` the first frame whatever the edit list or B-frame delay. The
  half-frame offset survives a container's rounded time base. D13's
  interrupt is terminal, so seeks must never use it.

**D28. Pixel formats, colour defaults, hardware fallback, the render node
and pooled downloads** (slice 2; updates D3, D14 and D18).
- **What:**
  - Native planar and semi-planar layouts at 8 to 16 bits upload as they
    are. Others go through swscale to `yuv444p` or `yuv444p16le`.
  - Unspecified colour fields follow a table, and each guess sets an
    `assumed` flag shown in the UI and JSON:
    - range from the frame, `yuvj` meaning full, else limited;
    - matrix BT.709 at width ≥ 1280 or height > 576, else BT.601 (mpv's
      and libplacebo's rule, since swscale assumes BT.601);
    - SDR transfer and BT.709 primaries;
    - Left siting by codec.
  - Fallback lives in the backend wrapper, at open and mid-stream, with
    bit-exact continuation. It is surfaced four ways, and Tier 2 uses
    `Fallback::Fail`.
  - VA-API opens on the render node of the GPU wgpu renders on: the PCI bus
    id, then a vendor and device scan, then renderD128.
  - Downloads use a 256-aligned `AVBufferPool`.
- **Why:** converting 4:2:2 or 4:4:4 to 4:2:0 on the CPU loses chroma and
  costs 10 to 30 ms per 5.7K frame. Untagged files are common, and the size
  rule is what players use. The flags let a person or an agent see a
  guess. D3 forbids silent fallback. Decoding and rendering on the same
  GPU avoids a PCIe copy and keeps M9's zero-copy possible on a machine
  with an iGPU. A fresh 25 to 50 MB allocation per frame is avoidable.

**D29. The render pipeline** (slice 3; updates D7).
- **What:**
  - Planes stay separate textures. 8-bit planes write straight into
    `R8Unorm`/`Rg8Unorm`. Deeper planes upload as `R16Uint`/`Rg16Uint`,
    and an ingest pass writes canonical `R16Float`/`Rg16Float` holding code
    / (2ᵇ − 1).
  - One area-weighted mip shader runs as render passes, capped near 64 px.
  - Gradients are analytic central differences of the view direction
    through the equirect Jacobian.
  - Goldens, the CPU comparison and `frame` sample isotropically with our
    own λ. The GUI uses 16× anisotropy by default, as a setting.
  - The sampler repeats U and clamps V, and the shader applies chroma
    siting offsets.
  - Colour goes through the sRGB EOTF into an `Rgba16Float` linear target,
    and an encode pass applies the OETF.
  - Upload starts with `write_texture`, with a measured trigger for the
    StagingBelt and the U1/U2/U3 ladder.
  - Slot reuse relies on wgpu's queue ordering, and an unchanged frame and
    view skip the projection.
  - PLAN's "decode to NV12/P010" and "sampling NV12/P010 planes directly"
    are reworded: decode to native planar or semi-planar YUV (NV12/P010
    from VA-API), upload unconverted, normalise deep planes on the GPU,
    convert to RGB only in the projection shader.
- **Why:**
  - Every format used is guaranteed on every driver. `R16Unorm` is not
    renderable without adapter-specific features, which would force a
    second, compute mip path.
  - f16 keeps 10-bit within a quarter code.
  - Capping the chain cuts 5.7K mip passes from 23 or 34 to 11 or 16.
  - Analytic gradients removed the measured seam and pole spikes (504, 344
    and 192 bad pixels down to 0).
  - Anisotropy is implementation-defined, so goldens can't use it.
  - `write_texture`'s cost was overstated in design and is measured
    before anything replaces it.
  - The intent of PLAN's wording, no CPU colour conversion, is kept.

**D30. Golden-image policy** (slice 3).
- **What:**
  - Goldens render on lavapipe from in-memory synthetic planes, plus a
    smaller decode-to-render set.
  - A manifest records the source-plane hash, `driver_info`, the wgpu
    version and the thresholds.
  - The match is exact at the same driver build and source hash, and
    otherwise PSNR ≥ 48 dB, SSIM ≥ 0.995 and at most 4 LSB.
  - The f64 CPU trilinear reference has a per-view tolerance calibrated at
    blessing (measured − 3 dB, never below 36 dB).
  - Minified views are compared with the supersampled ideal, with a margin
    over a level-0 control.
  - Seam and pole tests count bad pixels with `diff_stats`. The
    discontinuity is placed inside a 2×2 quad (asserted on the CPU), and
    each test has a roll twin and an implicit-LOD negative control.
  - Blessing refuses any image that fails its tolerance or whose controls
    pass.
  - Hardware GPUs are compared only with the CPU and ideal references.
  - The metrics are our own, in testkit.
- **Why:**
  - A golden that passes a broken implementation is worse than none: at
    yaw 179.85° or 180° the seam falls between quads and implicit LOD shows
    nothing.
  - Exact matching catches any change on the reference box, and tolerances
    absorb other Mesa builds.
  - The source hash keeps synth or libm drift from passing as a render
    change.
  - WGSL's loose `atan2` and `asin` make GPU-to-CPU error grow toward the
    rim, so tolerances are measured, not assumed.
  - dssim is AGPL-3.0.

**D31. View conventions** (slice 7, used from slice 4).
- **What:**
  - yaw > 0 turns right, pitch > 0 looks up, roll > 0 rolls the camera
    clockwise, with q = R_y(−yaw) · R_x(pitch) · R_z(−roll) and world =
    q · view.
  - A drag keeps the roll and puts the grabbed point under the cursor, or
    the nearest level orientation near a pole. Ctrl drags freely.
  - Zoom keeps the direction under the cursor.
  - Pointer positions are mapped in physical pixels with the paint
    callback's rounding.
  - The presets are rectilinear up to 170°, fisheye up to 360°, and tiny
    planet as stereographic 300° at pitch −90°.
  - sv3d crop bounds are applied. An sv3d pose is read and shown in a
    notice, but not applied. Stereo shows the left eye.
- **Why:** one convention serves the UI, `frame`, M2's keyframes and the
  MCP tools. A level horizon is what people expect when dragging 360
  video. Physical-pixel mapping makes "the point under the cursor stays
  under it" exact at fractional scales. Without the crop, padded or
  partial equirects would be misprojected. The Spherical Video V2 wording
  leaves the pose's direction ambiguous, so applying it waits for a tagged
  file checked against YouTube in M2.

**D32. The playback engine** (slice 5).
- **What:**
  - `spherecut-playback` runs decode (one per video stream), upload,
    audio-decode and audio-out threads, plus transient open, doctor and
    listing workers.
  - The `Player` takes commands that never block, plus a repaint callback.
  - The frame cache is a `BTreeMap` of `Arc<VideoFrame>` with run links.
    Its budget is clamp(0.25 × MemAvailable, 512 MiB, 8 GiB), capped by
    cgroup v2, configurable and rechecked. It has back-pressure, and
    catch-up when playback is late.
  - There are 4 GPU slots by default (3 to 6), reused only when free and
    unreferenced, relying on queue ordering.
  - The UI only `try_lock`s.
  - `may_block` is a runtime guard on top of the app's clippy bans.
  - Threads are joined after the event loop returns.
- **Why:**
  - PLAN requires that the UI never block, and a bounded, configurable
    cache.
  - An egui-free crate is what M2's export and the benches reuse.
  - `try_lock` turns contention into one repeated frame instead of a
    stall.
  - wgpu already orders a later write after an earlier draw on one queue.
  - Clippy configuration is per crate and cannot see other crates' code,
    so a runtime guard with a negative control covers the rest.

**D33. Audio output and the clock** (slice 6).
- **What:**
  - cpal `=0.18.2` with only `pulseaudio`, plus ALSA (always compiled on
    Linux); not `pipewire` or `jack`.
  - `BufferSize::Fixed(1024)`, or the nearest supported value.
  - The stream is owned by the `audio-out` thread.
  - A lock-free SPSC chunk ring in safe Rust, flushed by epoch.
  - A seqlock `AudioClock` from cpal's callback and playback timestamps.
  - Underrun debt; preroll of a ready video slot plus 100 ms of audio.
  - `SystemClock` when the device is lost or stalls for over 500 ms, and
    `NullOutput` with no device.
  - A static ambiX-to-stereo decode.
  - Video follows the audio clock.
- **Why:**
  - The pulse protocol host is pure Rust and reaches PipeWire on both
    distros. `pipewire` needs libpipewire-0.3 dev and libclang at build
    time (`--all-features` would not reach it, but no build machine should
    need them).
  - ALSA's default buffer can be very large.
  - Dropping a stream on the UI thread can block on the server.
  - Epochs flush without a lock in a real-time callback.
  - Debt keeps A/V aligned while video keeps moving.
  - With the DAC as the master clock there is no drift to correct.

**D34. GUI dependencies, features and Wayland gaps** (slice 7; updates
D21).
- **What:**
  - eframe without defaults: `wgpu_no_default_features`, `wayland`, `x11`,
    `default_fonts`, `persistence`, `clipboard`. winit with
    `wayland-dlopen` and without `wayland-csd-adwaita`.
  - `multisampling: 0` and `depth_buffer: 0`, with the device shared with
    egui through our adapter selector and device descriptor.
  - No `accesskit`, no `links` and no `rfd`: a Files pane instead.
  - kittest with `wgpu` only.
  - `[[bans.features]]` entries keep these choices from coming back
    through a dependency.
  - The fonts follow Pat's answer.
  - Recorded limits of winit 0.30.13: no touchpad pinch on Wayland or X11
    (scroll and Ctrl+scroll zoom instead), and no file drop on Wayland.
- **Why:** this graph passes advisories with no ignores. accesskit brings
  zbus's three unmaintained-crate advisories, and csd-adwaita brings
  ttf-parser's. The cost is no screen reader in M1 and a plain GNOME
  frame, revisited when upstream moves. Bundled fonts are a license
  question, which the prompt makes Pat's.

**D35. Hardware testing in M1** (slices 5 and 8; updates D19 and D20).
- **What:**
  - hw-test runs Tier 1 on the hardware machine.
  - It checks the ALSA headers, strace, rustc ≥ 1.95 and lavapipe.
  - It records the power profile, session, compositor settings and render
    nodes.
  - It generates noisy 100 Mbps 5.7K HEVC clips at 8 and 10 bits, flagged
    "too easy" if software decodes them above 150 fps.
  - It runs `gui_smoke` as a Tier 2 test that finds the Wayland socket even
    from a TTY. A hidden session fails, and a skip is never a pass.
  - Screenshots go into `hw-reports/`.
  - Pass rules: 0 drops after the first second over 60 s, a per-second
    decode minimum of 1.2× the frame rate, and in the GUI at most 0.1%
    drops, UI p99 under 1.5× refresh and |A/V| p95 under 20 ms.
  - Requests are batched as H1 (headless, after slice 5), H2 (GUI, after
    slice 8) and H3 (final).
- **Why:**
  - Synthetic grid content decoded at 330 fps in software, so it proved
    nothing about camera footage.
  - A per-second minimum catches the VA-API dips (1.17× to 2.41× on
    huginn) that a mean hides.
  - huginn's last report ran from a TTY.
  - FFmpeg 8.1.3 and Mesa 26 behave differently from this box's 6.1.1 and
    25.2.8.

**Updates to existing entries:**

- **D1:** MSRV 1.95 from slice 3, checked with `cargo +1.95`. Fedora 44's
  RPM is rustc 1.99 (huginn).
- **D3:** the fallback is surfaced four ways (D28).
- **D7:** deep planes upload as `R16Uint` and are sampled as f16, not
  `R16Unorm` (D29); the GUI uses anisotropic filtering.
- **D8:** the confinement and the `std::net` ban are done (D24).
- **D13:** a seek clears truncation and read failures, a refusal stays
  sticky, and the interrupt is for shutdown only (D27).
- **D15:** other containers are refused by name (D24).
- **D16:** `TempFile` moves to core. The CLI publishes without clobbering
  and renames only with `--overwrite` (D25).
- **D18:** the GUI runs the doctor at startup on the chosen adapter's
  render node, and the text formatter moves to `media::doctor`.
- **D19:** the report gains "Tier 1 on this machine" and a real GUI
  section.
- **D21:** feature bans join the crate bans (D34).
- **D22:** rows for every crate in section 11.

## 16. Open questions for Pat

Each comes with a recommendation, and work proceeds on the recommendation
where it can. Everything goes out in one batch in slice 1.

1. **Fonts** (a license decision; needed by slice 7). egui's bundled fonts
   are `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`, aggregated
   as data in the binary.
   - *Recommendation:* a `deny.toml` license exception scoped to the
     `epaint_default_fonts` crate. Both licenses allow bundling with any
     program, and Fedora and Debian ship OFL fonts beside GPL programs.
   - *Alternative:* turn `default_fonts` off and bundle GNU FreeFont
     (GPL-3.0-or-later with the font exception) through
     `FontDefinitions`. It is about 0.7 MB more and looks plainer.
2. **GNOME decorations, after you see H2's screenshots** (taste). Without
   winit's Adwaita feature, GNOME gets a plain fallback title bar.
   - *Recommendation:* the plain frame for M1.
   - *Alternatives:* the Adwaita look, with one documented
     unmaintained-crate advisory ignore (ttf-parser); or a title bar drawn
     by Spherecut in egui.
3. **Screen readers.** eframe's AT-SPI support (accesskit) brings three
   unmaintained-crate advisories through zbus.
   - *Recommendation:* ship M1 without screen-reader support, and revisit
     when accesskit moves to a maintained zbus.
   - If you need it now, it returns with documented ignores.
4. **Clip 12:** a stitched 5.7K30 equirect export of clip 1, plus an 8K one
   if you can. Insta360 Studio does not run on Linux, so the phone app's
   export, or Studio on another computer, will do. M1's real-time
   criterion is about stitched footage, and the synthetic clips only stand
   in for it.
5. **odin:** please run `cargo xtask hw-test` on current main, from a
   terminal inside GNOME. It would be odin's first report.
6. **Hyprland:** can huginn or odin log into a Hyprland session (Fedora
   packages it)? Otherwise the Hyprland check waits for the Framework, and
   that bullet stays "done pending hardware".

## Appendix A. What the judges found, and where this plan fixes it

| Finding | Fixed in |
|---|---|
| performance and correctness: no FFmpeg confinement | 3.6, D24, slice 1 |
| performance and correctness: no JSON contract; no exit codes 3 and 4; no `--overwrite`; the rename silently overwrites | 5, 5.1, D25, slices 1 and 4 |
| performance and correctness: decision numbers collide with D23 | 15 starts at D24 |
| performance: `write_texture`'s cost overstated (no per-frame `vkAllocateMemory`) | 1.3, 4.4: measured first, with a stated trigger |
| performance: "lock contention" is speculative | dropped as an argument; UI p99 is measured in `gui_smoke` |
| performance: 16× anisotropy makes goldens driver-variable | 4.3, D29, D30: isotropic for goldens, the CPU comparison and `frame`; aniso is a GUI setting |
| performance: seek algorithm unprobed | 3.3, D27: seekprobe's measured re-seek, with the oracle and its negative control |
| all three: the `--all-features`/pipewire reasoning | 1.3, D33: the real reason is the build-time libraries |
| performance: `HIGH_THROUGHPUT` is redundant | 1.3, 7.1: the default is left alone |
| performance: `AVFrame.duration` version | 1.3: libavutil 57.43 |
| performance: the seam golden at yaw 180° sits on a quad boundary (also simplicity) | 6: the yaw is computed so the seam falls inside a quad, asserted on the CPU, with the implicit-LOD control |
| performance: "f32 atan2 ~1e-7 rad" | 1.3, 4.3: tolerances calibrated per view |
| performance: most complex design | 4 slots, one mip shader, `write_texture` baseline, no retirement callbacks |
| correctness: FFmpeg's matrix heuristic misattributed | 3.5, D28: mpv's rule; the swscale cross-check passes the matrix explicitly |
| correctness: MSAA from `RenderState` | 7.1: `NativeOptions.multisampling` |
| correctness: "unsafe only in ffi.rs" ignores hwprobe.rs | 2.4, D26 |
| correctness: `clippy.toml` scoping, wildcards and lost test allows | 7.9: exact paths, per crate, repeating the allows and the `std::net` ban; the runtime guard covers the rest |
| correctness and simplicity: no Wayland pinch noticed | 7.4, D34 |
| correctness: `--adapter hw` default breaks GPU-less boxes | 4.1, 5: Auto falls back to lavapipe |
| correctness: the uploader reaching `Viewer` inside `CallbackResources` | 7.1, 8.3: `SlotRing` in an `Arc` outside egui's renderer lock |
| correctness: a retirement callback guards a hazard wgpu already orders | 4.4 |
| simplicity: `from_points` with `[u32::MAX; 2]` panics | 7.3: the real `ScreenDescriptor.size_in_pixels` |
| simplicity: `Event::Screenshot` does not exist | 12.4: `ctx.request_screenshot` |
| simplicity: doctor without HEVC exits 3 | 5.1, D25: doctor's errors exit 1 |
| simplicity: the time object lacks `frame` | 5.1, D25: `FrameTime` |
| simplicity: chroma siting ignored | 3.5, 4.3, `siting_left_center` |
| simplicity: no anisotropy | 4.3: the GUI default |
| simplicity: CPU swscale of 4:2:2, 4:4:4 and 12-bit to 4:2:0 | 3.4: native upload; swscale only for exotic formats, to 4:4:4 |
| simplicity: renderD128 hard-coded | 3.5: the adapter's PCI bus id |
| simplicity: UI `lock()`s `Mutex<EngineState>` | 8, 7.9: `try_lock` only, clippy bans, `may_block` |
| simplicity: easy perf clip | 12.4: noisy 100 Mbps clip with the "too easy" flag |
| simplicity: mip pass counts | 4.2: 11 and 16 passes with the cap (23 and 34 uncapped) |
| simplicity: sv3d crop deferred | 4.3, D31: crop applied in M1 |
| simplicity: strace prerequisite, README, D15 and `lrc` changes missing | 3.6, 12.4, slice 1 |
| simplicity: Fedora 44's RPM is 1.99 | 1.3, D26, D1 update |
| simplicity: the grab works in points | 7.4, D31: physical pixels with the callback's rounding |
| simplicity: `BufferSize::Default`; a stream dropped per seek on the UI thread | 9.2, 8.1, D33 |

## Appendix B. Deliberately not in M1 (listed in PROGRESS as unavailable)

| Item | When |
|---|---|
| Zero-copy VA-API to Vulkan (U3) | M9, or earlier if risk 1 bites |
| Applying the sv3d pose | M2, with a tagged sample |
| HDR tone mapping, BT.2020 gamut mapping, BT.1886 | M7 |
| Vulkan Video in Auto | when a driver decodes HEVC with it (huginn's RADV decodes only AV1) |
| Audio while scrubbing, JKL shuttle, recent files | M5 |
| Binaural ambisonics steered by the view | M6 |
| Screen readers, Adwaita decorations | Pat's answers (section 16) |
| A native PipeWire audio host | when every build machine has libpipewire dev |
| Dual-fisheye, EAC and other unstitched sources (refused with a message) | M3 |
