# Spherecut architecture

This describes the design as built, plus the planned parts marked
*(planned)*. DECISIONS.md records why each choice was made.

## Crates

```
          app (egui GUI, planned)   cli (`spherecut` binary)   tests
                          \                |                    /
                           +------ export, audio (planned) ----+
                                      |
                             render (wgpu, WGSL)  (planned)
                                      |
             motion (IMU, stabilization)   stitch (lens models, blending)  (planned)
                                      |
                       camera (insta360, gopro, dji)  (planned)
                                      |
             media (FFmpeg, probing, MP4 boxes, doctor; MediaBackend planned)
                                      |
                 sphere (projection math)      core (time; project model planned)
```

`synth` (test patterns and media) and `testkit` (test helpers) sit beside
this stack and are used only by tests and `xtask`.

| Crate | Role | Status |
|---|---|---|
| `spherecut-core` | Product name constants (`brand`), exact rational time; later the project model, timeline, keyframes, undo | M0: name and time only |
| `spherecut-sphere` | CPU reference projection math in f64: equirect, cube/EAC, fisheye lens models, view projections and morphs, rotation helpers | M0 |
| `spherecut-media` | FFmpeg in a safe RAII layer, probing with structural damage checks, our own ISO-BMFF box reader and metadata injector, a sequential video decoder, a small MP4 writer, `doctor`'s capability checks. The `MediaBackend` trait is planned | M0; `MediaBackend` in M1 |
| `spherecut-synth` | Synthetic scenes (grid, high-frequency, seam), YUV conversion, tones and first-order ambiX, test clips and broken files | M0 |
| `spherecut-testkit` | Tier 2 gating (`tier2::require_*`, the `TIER2-SKIP:` marker) and benchmark recording (`bench`) | M0 |
| `spherecut-camera` | Per-camera raw formats: file pairing, vendor metadata, calibration | M3 |
| `spherecut-stitch` | Lens calibration models, seam blending, exposure matching | M3 |
| `spherecut-motion` | IMU ingest, time sync, orientation fusion, stabilization | M4 |
| `spherecut-render` | wgpu device setup, render graph, projection/color shaders, scopes, golden-image harness | M1 |
| `spherecut-audio` | Playback clock, mixing, ambisonics, loudness | M1 (playback), M6 |
| `spherecut-export` | Encode pipeline, render queue, metadata injection and validation | M2 |
| `spherecut-app` | eframe app, docked panels, viewer paint callbacks | M1 |
| `spherecut-cli` | The `spherecut` binary: `probe` and `doctor` today. `frame` (M1), `render` (M2) and the GUI when run without a subcommand (M1) are planned | M0: `probe`, `doctor` |
| `xtask` | `check`, `gen-media`, `hw-test` | M0 |

Crates are created when they get real content, not as empty shells. The
planned ones above don't exist yet.

## Media crate (as built in M0)

`unsafe` code lives in two modules, each block with a SAFETY comment:

* `ffi.rs` holds what ffmpeg-next doesn't cover safely: codec-parameter
  fields (colour, channel layout, display matrix, profile), stream media
  types, hardware device types and codec hardware configs, `HwDevice` (a
  reference-counted device context, released on drop) and `HwError`, the
  `get_format` callback that refuses software fallback and records why
  (`HwRefusal`, kept in the codec context's `opaque`), opening a video
  decoder by its int codec id, reading a frame's format and planes as
  ints, copying hardware frames to system memory, the demuxer index end,
  setting other streams to `AVDISCARD_ALL`, library versions, and the
  bridge that sends FFmpeg's `av_log` to `tracing` under the target
  `ffmpeg`. C enums are never made into bindgen's Rust enums (DECISIONS
  D11).
* `hwprobe.rs` (private) holds doctor's one-frame hardware decode and
  encode probes, the Vulkan device listing through FFmpeg's
  `AVVulkanDeviceContext` with its own `#[repr(C)]` declarations of the
  few Vulkan structs it reads, and the capture of what FFmpeg logs during a
  check (a thread-local `tracing` subscriber that passes everything on).

The other modules are safe Rust:

* `probe`: what FFmpeg reports, plus our own checks. MP4/MOV files go
  through `mp4::Mp4Tree::damage()` (a top-level box cut off by the end of
  the file, including a cut header; malformed boxes; a handler that
  disagrees with its sample entry; sample tables checked against each
  other and against the `mdat` ranges). Each FFmpeg stream is matched to
  its `trak` by track ID (by position when IDs repeat or are missing), and
  a video or audio `trak` that FFmpeg reads as another kind is damage. A file FFmpeg
  reads as MOV/MP4 that the strict read refuses is read again with
  `FirstBox::Any`, which accepts any valid first box. Matroska files are
  checked against their Segment length. `ProbeReport.structure` records
  how much was checked (`full`, `length` or `none` with a reason), and
  `damage` lists what was found. The CLI fails on any damage (D15).
* `mp4`: the box reader, with fixed limits (DECISIONS D15).
* `spherical`: Spherical Video V1 and V2 and SA3D, read from the boxes and
  written as box bytes.
* `inject`: adds or replaces boxes in the first video and audio sample
  entries, fixing sizes and chunk offsets, written atomically (D16).
* `encode`: `Mp4Writer`, one video stream from planar YUV and optional
  planar-f32 audio, written atomically like `inject`. The synthetic media
  generator uses it today; M2's export builds on it.
* `decode`: `VideoDecoder`, which decodes one video stream in order, in
  software or on a `HwDevice`, and hands out frames in system memory. It
  reads only its own stream (the others are discarded), holds back a
  packet flagged corrupt until the next read, and ends a damaged file with
  an error after the frames it could decode (D13). `open_cancellable`
  stops reading when a flag is set, and `skipped_packets` counts packets
  skipped as invalid data. On a device it never falls back to software
  (D3, D14).
* `detect`: camera and layout guesses from container structure, each with
  its evidence.
* `doctor`: the capability checks and the fixes (D18). The embedded
  one-frame bitstreams are in `crates/media/fixtures/`, whose README gives
  the commands that regenerate them byte for byte.

`MediaError` has six variants: `Io`, `NotAFile`, `Open`, `Decode`,
`HwUnsupported { path, kind, refusal: HwRefusal }` (the device can't decode
this stream; software may) and `Hardware { kind, reason: HwError }` (the
device can't be opened). Messages that include their cause don't also
return it from `source()`, so `{e:#}` prints each cause once.

`spherecut_media::init()` initialises FFmpeg once and installs the log
bridge. FFmpeg's own log level follows what the `tracing` subscriber
accepts for target `ffmpeg`, so a subscriber installed after `init()`
should call `sync_ffmpeg_log_level()`.

## Coordinates and conventions

* World and camera space are right-handed: **+X right, +Y up, −Z forward**.
  The centre of an equirect frame is forward.
* Longitude `atan2(x, −z)`, latitude `asin(y)`. Equirect `u = 0.5 + lon/2π`,
  `v = 0.5 − lat/π`, `v = 0` at the top.
* Normalised image coordinates `(u, v) ∈ [0, 1]²`; texel centres at
  half-integers.
* Stored rotations are unit quaternions (`glam::DQuat` on the CPU). A view
  orientation `q` maps view-space rays to world space: `world = q * view`.
  The UI may show yaw/pitch/roll, derived on display.
* A lens or virtual camera looks down its own −Z axis.

## Time

All stored time is exact (`spherecut_core::time`):

* `Rational`: reduced `i64/i64`, checked arithmetic in `i128`.
* `Time`: seconds as a `Rational`; timeline positions are `Time`.
* `FrameRate`: positive `Rational` (`30000/1001` for 29.97).
* Streams keep integer PTS in their own timebase; conversion to and from
  `Time` is explicit about rounding. Variable-frame-rate sources are
  addressed by PTS, never by frame index.

## Rendering *(planned, M1)*

Every source type implements, in WGSL, "direction on the unit sphere →
texel(s)": equirect, the GoPro EAC pair, dual fisheye with calibration. For
each output pixel the shader:

1. builds a ray from the output projection (rectilinear / fisheye /
   stereographic with morph, `sphere::perspective` is the reference),
2. rotates it by the keyframed view orientation and the stabilization
   rotation,
3. maps it into each contributing source and samples the **YUV planes
   directly** (NV12: R8 + RG8, P010: R16 + RG16), converting with the
   source's matrix and range,
4. blends lenses across the seam where relevant, converts to linear light
   and runs the color pipeline in float, then writes the output.

Stitching, stabilization and reframing therefore happen in one pass, shared
by the viewer and the exporter. An intermediate equirect exists only as the
output of a 360 export.

The ±180° seam and the poles break hardware derivative-based LOD, so the
shader computes LOD analytically from the output pixel's angular footprint
and samples with `textureSampleGrad`/`textureSampleLevel`.

## Threads *(planned, M1)*

* UI thread: egui, never blocks on decode, disk or GPU readback.
* Decode workers per open source, feeding a bounded frame cache.
* Audio thread (cpal): owns the playback clock; video presentation follows
  it.
* Background workers for thumbnails, waveforms, proxies and export, each
  with progress reporting and cancellation tokens.

## Testing tiers

* **Tier 1** runs anywhere: unit and property tests, software decode and
  encode, synthetic media that the tests generate themselves, and doctor
  run against lavapipe. Golden images on lavapipe come with the renderer
  (M1). `cargo xtask check` runs all of it.
* **Tier 2** needs hardware or samples. Tier 2 tests are `#[ignore]`d with a
  reason and call `spherecut_testkit::tier2::require_*`, which fails the
  test with a `TIER2-SKIP:` marker when a device or sample is missing.
  `cargo xtask hw-test` first lists the Tier 2 tests with `cargo test
  [--release] -- --ignored --list`, then runs them with `--no-fail-fast`,
  and reports every listed test exactly once as `<binary>::<test>`: pass,
  FAIL with its output, skipped with the reason, left out by `--quick`, or
  did not run. Each test binary's exit is judged on its own, so a binary
  that crashed or died after its tests finished fails the run even when
  another binary's skip explains cargo's failure. A test that did not run,
  a failure, or a failed listing marks the run FAILED. DECISIONS D19 has
  the details.
