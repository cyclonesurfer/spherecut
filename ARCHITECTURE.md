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
| `spherecut-agent` | Agent integration (D23): the request and response type of each operation, shared by `spherecut mcp` and `spherecut call` (from A1 the `--json` of `probe`, `doctor` and `frame` uses them too, with a schema version bump if M1's shape changes); the MCP server; later the GUI attach bridge | A1 (after M1) |
| `spherecut-cli` | The `spherecut` binary: `probe` and `doctor` today. `frame` (M1), `render` (M2), the GUI when run without a subcommand (M1), and `mcp` and `call` (A1) are planned | M0: `probe`, `doctor` |
| `xtask` | `check`, `gen-media`, `hw-test` | M0 |

Crates are created when they get real content, not as empty shells. The
planned ones above don't exist yet.

## Media crate (as built in M0, with M1's confinement of FFmpeg)

`unsafe` code lives in two modules, each block with a SAFETY comment:

* `ffi.rs` holds what ffmpeg-next doesn't cover safely: codec-parameter
  fields (colour, channel layout, display matrix, profile), stream media
  types, hardware device types and codec hardware configs, `HwDevice` (a
  reference-counted device context, released on drop) and `HwError`, the
  `get_format` callback that refuses software fallback, opening files
  with FFmpeg confined to them, reading metadata, finding a stream's
  decoder, opening decoders under a picture size limit and a video
  decoder by its int codec id, reading a frame's format and planes as
  ints, copying hardware frames to system memory, the demuxer index end,
  setting other streams to `AVDISCARD_ALL`, library versions, and the
  bridge that sends FFmpeg's `av_log` to `tracing` under the target
  `ffmpeg`. C enums are never made into bindgen's Rust enums (DECISIONS
  D11). In more detail:
  * The log bridge sends one event per line, each with the `[hevc @
    0x...]` prefix of its context. A line FFmpeg logs in several calls
    waits in a thread-local buffer until a call ends it, and a call that
    holds several lines gives each of them its prefix (D12).
    `end_log_line` sends a line that never got its end.
  * The `get_format` callback keeps its bookkeeping in the codec context's
    `opaque`: the device's pixel format, and flags that describe the
    latest call. Asked: FFmpeg has called it at all. Taken: the latest
    call offered the device's format and it was taken. Decoded: a frame
    has come out on the device since. Withdrawn: the latest call lacked
    the format right after a call that took it, with no device frame in
    between. `hw_refusal` reads them as a `HwRefusal` (D14).
  * Given a device, `open_video_decoder` takes the first decoder for the
    codec whose hardware configs offer `hw_device_ctx` for the device's
    type, not FFmpeg's default decoder: for AV1 that is libdav1d, which
    only decodes in software.
  * A slice from `plane` ends inside the `AVBufferRef` that holds the
    plane, so a frame the decoder cropped on the left can't make it run
    past its buffer.
  * `open_input` opens every file Spherecut demuxes, and holds FFmpeg to
    that file (D24). It refuses a path that isn't a regular file, and
    gives FFmpeg `file:` followed by the path's raw bytes, so a name like
    `concat:x.mp4` is never read as a URL (and a path that isn't UTF-8
    works, where ffmpeg-next's `format::input` panics). It identifies the
    demuxer first (`av_probe_input_buffer2` under the file protocol) and
    refuses any outside `DEMUXERS`, the MOV/MP4 family and
    Matroska/WebM, by name (`OpenError::Container`), before that demuxer
    runs. It then opens the file with that demuxer, `protocol_whitelist`
    `file`, a `format_whitelist` of `DEMUXERS`, MOV's `enable_drefs` 0
    and its own `io_open`, `open_only_the_file`, which lets FFmpeg's
    `io_open` open only the context's own input and refuses every other
    open with EPERM. A thread-local list (`RefusalWatch`) names what was
    refused during the open (`OpenError::Nested`). The decoders
    `avformat_find_stream_info` opens get `MAX_PIXELS` through its
    per-stream options. When one of them refuses a stream's size and
    FFmpeg leaves the stream at 0x0, `open_input` puts back the size the
    demuxer read from the container, so that callers can name it.
  * `open_decoder` opens every decoder (`probe`, `VideoDecoder`,
    `open_video_decoder`, doctor's probes) with `max_pixels` =
    `MAX_PIXELS`, 2^27 pixels (16384 × 8192). `TooLarge::of` says
    whether a stream's size is over it, with the width padded to 64 as
    FFmpeg pads rows before it checks; `probe` warns of such a stream and
    `VideoDecoder` refuses it (`MediaError::PictureTooLarge`), by its
    size, before any decoder fails on it. `clippy.toml` forbids
    ffmpeg-next's own input, output and decoder opens (`format::open`,
    `format::input*`, `format::output*`, `Decoder::open*` and the like),
    and `std::net`'s TCP and UDP types, elsewhere.
  * `dict_entries` reads metadata as lossy UTF-8 (U+FFFD for bytes that
    aren't), because ffmpeg-next makes `&str`s of it unchecked.
* `hwprobe.rs` (private) holds doctor's one-frame hardware decode and
  encode probes, the Vulkan device listing through FFmpeg's
  `AVVulkanDeviceContext` with its own `#[repr(C)]` declarations of the
  few Vulkan structs it reads, and `capture_ffmpeg_log`, which collects
  what FFmpeg logs while doctor runs a check or `probe` reads a file (a
  thread-local `tracing` subscriber that passes everything on). It ends
  any unfinished line before and after, so that each captured line is
  whole and is the run's own.

The other modules are safe Rust:

* `probe`: what FFmpeg reports, plus our own checks. MP4/MOV files go
  through `mp4::Mp4Tree::damage()`: a top-level box cut off by the end of
  the file, including a cut header; malformed boxes, among them a box
  whose size is 0 below the top level, which the standard allows only for
  the last top-level box (in `moov` and the small top-level containers
  the reader loads, such as `udta` and `meta`; fragments are not read); a
  handler that disagrees with its sample entry; sample tables checked
  against each other and against the `mdat` ranges; a box type that isn't printable
  text where only registered types belong (in `moov`, `trak`, `mdia`,
  `minf`, `stbl`, `stsd`, `edts`, `dinf`, `mvex`, `tref`, the known sample
  entries, `sv3d` and `proj`),
  where four zero bytes pass only as a sample entry's all-zero last box,
  QuickTime's terminator; a `moov` child under another printable type
  whose payload starts with a `tkhd` (a renamed `trak`, whose payload is
  kept whatever its size); an `sv3d` without `svhd`, `proj` or
  `proj/prhd`, which FFmpeg ignores, and a sample entry's child under
  another printable type whose payload starts with an `svhd` (a renamed
  `sv3d`); and a `mvhd` or `mdhd` timescale FFmpeg can't use. Each FFmpeg
  stream is matched to its `trak` by track ID (by position when IDs repeat
  or are missing), and a video or audio `trak` that FFmpeg reads as
  another kind is damage. A video or audio stream FFmpeg could not set up
  (no codec, frame size, sample rate or channels, or no pixel or sample
  format though FFmpeg has a decoder, which `ffi::find_decoder` looks up
  by the codec id as an int) is damage when its track has samples and its
  sample entry is a codec FFmpeg reads (`mp4::is_known_codec`), and a
  warning otherwise, since FFmpeg may simply lack the codec. Otherwise
  probe opens the stream's decoder from its codec parameters, on one
  thread, and closes it: a refusal is damage, unless the error says FFmpeg
  lacks a feature (`ENOSYS`, `AVERROR_PATCHWELCOME`,
  `AVERROR_EXPERIMENTAL`), and a codec with no decoder in this build is a
  warning. `probe::Undecodable` holds the reason and decides which it is.
  A file FFmpeg reads as MOV/MP4 that the strict read refuses is read
  again with `FirstBox::Any`, which accepts any valid first box. Matroska
  files are checked against their Segment length. No other container
  gets this far: `open_input` refuses them by name (D24). `probe` runs
  inside `hwprobe::capture_ffmpeg_log` and keeps FFmpeg's warnings and
  errors in `ProbeReport.ffmpeg_log`, apart from `damage` and `warnings`,
  or in `ProbeError.ffmpeg_log` beside the `MediaError` when it fails.
  `ProbeReport.structure` records how much was checked (`full`, `length`
  or `none` with a reason), and `damage` lists what was found.
  `ProbeReport.path` serializes as lossy UTF-8. The CLI fails on any
  damage (D15).
* `mp4`: the box reader, with fixed limits (DECISIONS D15).
* `spherical`: Spherical Video V1 and V2 and SA3D, read from the boxes and
  written as box bytes.
* `inject`: adds or replaces boxes in the first video and audio sample
  entries, fixing sizes and chunk offsets, written atomically (D16). It
  refuses input that `Mp4Tree::damage()` finds damaged, and, when it
  shifts chunk offsets, an edited `moov` whose re-read warns. It replaces
  only a regular file at the output.
* `encode`: `Mp4Writer`, one video stream from planar YUV and optional
  planar-f32 audio, written atomically like `inject`, which also means
  it refuses an output that isn't a regular file. The synthetic media
  generator uses it today; M2's export builds on it.
* `decode`: `VideoDecoder`, which decodes one video stream in order, in
  software or on a `HwDevice`, and hands out frames in system memory. It
  reads only its own stream (the others are discarded), holds back a
  packet flagged corrupt until the next read, and ends a damaged file with
  an error after the frames it could decode (D13). `open_cancellable`
  stops reading when a flag is set, and `skipped_packets` counts packets
  skipped as invalid data. On a device it opens a decoder that can use the
  device and never falls back to software (D3, D14).
* `detect`: camera and layout guesses from container structure, each with
  its evidence.
* `doctor`: the capability checks and the fixes (D18). A failed check
  names its `fault`: this FFmpeg build (`ffmpeg`), the driver refusing or
  failing (`driver`), or the driver giving a wrong picture or stream
  (`wrong_output`). The embedded one-frame bitstreams are in
  `crates/media/fixtures/`, whose README gives the commands that
  regenerate them byte for byte.

`MediaError` has seven variants: `Io`, `NotAFile`,
`UnsupportedContainer { path, format, description }` (FFmpeg would read
the file with a demuxer outside `DEMUXERS`; the message names it and the
containers Spherecut opens), `Open` (which includes a nested open
`io_open` refused), `Decode`,
`HwUnsupported { path, kind, refusal: HwRefusal }` (the device can't decode
this stream; software may) and `Hardware { kind, reason: HwError }` (the
device can't be opened). `VideoDecoder::open` returns `HwUnsupported`
(`NotOffered`) when no decoder for the codec can use the device, and
`next_frame` returns it when the device turns the stream down. A software
frame from a decoder that never asked for the device's format, or that
turned it down, is `HwUnsupported` too, not a `Decode` error. Once
`next_frame` has returned it, every later call returns it again and
decodes nothing. Messages that include their cause don't also return it
from `source()`, so `{e:#}` prints each cause once.

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
  encode, synthetic media that the tests generate themselves, doctor run
  against lavapipe, and `VideoDecoder` turning streams down on lavapipe's
  Vulkan device. Golden images on lavapipe come with the renderer (M1).
  `cargo xtask check` runs all of it.
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
