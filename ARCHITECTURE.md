# Spherecut architecture

This describes the design as built, plus the planned parts marked
*(planned)*. DECISIONS.md records why each choice was made.

## Crates

```
          app (egui GUI, planned)   cli (`spherecut` binary)   tests
                          \                |                    /
                           +------ export, audio (planned) ----+
                                      |
                    render (wgpu on Vulkan, WGSL; M1 slice 3a)
                                      |
             motion (IMU, stabilization)   stitch (lens models, blending)  (planned)
                                      |
                       camera (insta360, gopro, dji)  (planned)
                                      |
     media (FFmpeg, MediaBackend and frame-exact seek, probing, MP4 boxes, doctor)
                                      |
   sphere (projection math)      core (time, frames, colour, stop tokens; project model planned)
```

`synth` (test patterns and media, and backends that stand in for FFmpeg)
and `testkit` (test helpers) sit beside this stack and are used only by
tests and `xtask`.

render depends on core and sphere only, not on media: frames reach it
through `core::video::VideoPlanes`, so its tests, the goldens and M3's
stitcher feed it without FFmpeg.

| Crate | Role | Status |
|---|---|---|
| `spherecut-core` | Product name constants (`brand`), exact rational time and its serialized form (D27), what a decoded frame is (`video`: pixel layouts, planes, colour) and stop tokens by generation (`cancel`); later the project model, timeline, keyframes, undo | M0: name and time; M1: `video`, `cancel` |
| `spherecut-sphere` | CPU reference projection math in f64: equirect and its uv gradient, cube/EAC, fisheye lens models, view projections and morphs and their inverse (`project_morph`), output pixels to lens coordinates (`pixel`), yaw, pitch and roll (`orient`), rotation helpers | M0; M1 slice 3a: `pixel`, `orient`, `uv_gradient`, `project_morph` |
| `spherecut-media` | FFmpeg in a safe RAII layer, probing with structural damage checks, our own ISO-BMFF box reader and metadata injector, a sequential video decoder, the `MediaBackend` traits with frame-exact seek in software and on VA-API or Vulkan Video with fallback to software, the render node lookup, the colour default rules, a small MP4 writer, `doctor`'s capability checks | M0; `MediaBackend` from M1 slice 2a, hardware from 2b (audio in 6) |
| `spherecut-synth` | Synthetic scenes (grid, high-frequency, seam), YUV conversion at any matrix, range and subsampling, tones and first-order ambiX, test clips (open GOPs, edit lists, VFR, Matroska remuxes, seeded noise for camera bitrates) and broken files, `SyntheticBackend` and `GatedBackend` | M0; M1 slices 2a and 2b |
| `spherecut-testkit` | Tier 2 gating (`tier2::require_*`, the `TIER2-SKIP:` marker), benchmark recording (`bench`, with latency percentiles), image comparison (`compare`: PSNR, SSIM, per-pixel difference statistics) and FNV-1a hashing (`fnv64`) | M0; M1 slice 3a: `compare`, `fnv64` |
| `spherecut-camera` | Per-camera raw formats: file pairing, vendor metadata, calibration | M3 |
| `spherecut-stitch` | Lens calibration models, seam blending, exposure matching | M3 |
| `spherecut-motion` | IMU ingest, time sync, orientation fusion, stabilization | M4 |
| `spherecut-render` | The wgpu device (Vulkan only), canonical plane textures with area mip chains, the upload path, the projection and encode shaders with colour from `ColorInfo`, headless stills and readback, and the f64 CPU reference the tests hold the GPU to (D31); later the golden-image harness (slice 3b) and scopes | M1 slice 3a |
| `spherecut-audio` | Playback clock, mixing, ambisonics, loudness | M1 (playback), M6 |
| `spherecut-export` | Encode pipeline, render queue, metadata injection and validation | M2 |
| `spherecut-app` | eframe app, docked panels, viewer paint callbacks | M1 |
| `spherecut-agent` | Agent integration (D23): the request and response type of each operation, shared by `spherecut mcp` and `spherecut call` (from A1 the `--json` of `probe`, `doctor` and `frame` uses them too, under a new schema version if M1's shape changes, D27); the MCP server; later the GUI attach bridge | A1 (after M1) |
| `spherecut-cli` | The `spherecut` binary: `probe` and `doctor` today. `frame` (M1), `render` (M2), the GUI when run without a subcommand (M1), and `mcp` and `call` (A1) are planned. Its library half holds `contract`, the JSON contract of `--json` (D27), until A1 moves it to `spherecut-agent` or a crate below both | M0: `probe`, `doctor`; M1: the JSON contract |
| `xtask` | `check`, `gen-media`, `hw-test` | M0 |

Crates are created when they get real content, not as empty shells. The
planned ones above don't exist yet.

## Media crate (as built in M0, with M1's confinement of FFmpeg and `MediaBackend`)

`unsafe` code lives in two modules, each block with a SAFETY comment:

* `ffi.rs` holds what ffmpeg-next doesn't cover safely: codec-parameter
  fields (colour, channel layout, display matrix, profile), stream media
  types, hardware device types and codec hardware configs, `HwDevice` (a
  reference-counted device context, released on drop) and `HwError`, the
  `get_format` callback that refuses software fallback, opening files
  with FFmpeg confined to them, reading metadata, finding a stream's
  decoder, opening decoders under a picture size limit and a video
  decoder by its int codec id, reading a frame's format, planes,
  timestamps, flags and colour codes as ints (`frame_fields`), the
  layout a pixel format is uploaded in (`pixel_layout`, from
  `AVPixFmtDescriptor`) and the format swscale converts the others to
  (`conversion_target`, `Scaler`), copying hardware frames to system
  memory (`download_into_pool`, into a `DownloadPool`, and doctor's
  `download_frame`) and the surfaces a hardware decoder keeps spare
  (`set_extra_hw_frames`), `av_seek_frame` and the demuxer index (its end, its first,
  last and shown entries, its timestamps, the keyframe before a time),
  skipping non-reference frames, the bytes a frame's buffers take
  (`buffer_bytes`), setting other streams to `AVDISCARD_ALL`, library versions, and the
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
    `MAX_PIXELS`, 2^27 pixels (16384 × 8192), and with
    `AV_CODEC_FLAG_UNALIGNED`, so a frame is cropped exactly as its
    stream says, not by as much as keeps its planes aligned (D30).
    `TooLarge::of` says whether a stream's size is over it, with the
    width padded to 64 as FFmpeg pads rows before it checks; `probe`
    warns of such a stream and `VideoDecoder` refuses it
    (`MediaError::PictureTooLarge`), by its size, before any decoder
    fails on it. `clippy.toml` forbids
    ffmpeg-next's own input, output and decoder opens (`format::open`,
    `format::input*`, `format::output*`, `Decoder::open*` and the like),
    and `std::net`'s TCP and UDP types, elsewhere.
  * `dict_entries` reads metadata as lossy UTF-8 (U+FFFD for bytes that
    aren't), because ffmpeg-next makes `&str`s of it unchecked.
  * `download_into_pool` copies a device frame into a buffer of an
    `AVBufferPool` built for the frames context's `sw_format` and the
    frame's size: every plane in one buffer, rows 256 bytes apart and
    planes on 256-byte boundaries, so wgpu's `write_texture` can take
    them in one copy. The buffer returns to the pool when the frame's
    last reference drops, and a new format or size rebuilds the pool.
    The pool's allocator counts the buffers it makes, which
    `DecodeStats::download_buffers` gives.
    The left and top crop FFmpeg leaves on device frames is applied after
    the copy, exactly, as software decoding applies it (`as_downloaded`
    gives the fields the frame will have). It serves VA-API and Vulkan
    alike (D30).
* `hwprobe.rs` (private) holds doctor's one-frame hardware decode and
  encode probes, the Vulkan device listing through FFmpeg's
  `AVVulkanDeviceContext` with its own `#[repr(C)]` declarations of the
  few Vulkan structs it reads, and `capture_ffmpeg_log`, which collects
  what FFmpeg logs while doctor runs a check or `probe` reads a file (a
  thread-local `tracing` subscriber that passes everything on, beside one
  that wants nothing and lives as long as the process, so that `tracing`
  asks every subscriber, the capture's among them, whether it wants a
  callsite another thread reaches first). It ends
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
  files are checked against their Segment length, also when FFmpeg can't
  open them, so that one cut short before its tracks is `truncated`. No other container
  gets this far: `open_input` refuses them by name (D24). `probe` runs
  inside `hwprobe::capture_ffmpeg_log` and keeps FFmpeg's warnings and
  errors in `ProbeReport.ffmpeg_log`, apart from `damage` and `warnings`,
  or in `ProbeError.ffmpeg_log` beside the `MediaError` when it fails.
  `ProbeReport.structure` records how much was checked (`full`, `length`
  or `none` with a reason), and `damage` lists what was found. Each
  entry of `damage` and `warnings` is a `Problem`, a stable code from
  `probe::codes` with a message (D27): damage is `truncated` for the
  cut itself and `damaged` otherwise. `ProbeReport::failure()` says why
  a report fails the file, and `ProbeError::code()` why the file could
  not be read, refining the `MediaError`'s code from what probe found
  (a file FFmpeg can't open is `truncated`, `damaged` or `not_media` when
  probe can tell why, and `open_failed` otherwise). A read error of the
  file itself is `MediaError::Io`, never damage.
  `ProbeReport.path` serializes as lossy UTF-8, and its durations and
  start time as `Time`s. The CLI fails on any damage (D15).
* `mp4`: the box reader, with fixed limits (DECISIONS D15).
* `spherical`: Spherical Video V1 and V2 and SA3D, read from the boxes and
  written as box bytes.
* `inject`: adds or replaces boxes in the first video and audio sample
  entries, fixing sizes and chunk offsets, written atomically (D16). It
  refuses input that `Mp4Tree::damage()` finds damaged, and, when it
  shifts chunk offsets, an edited `moov` whose re-read warns. It replaces
  only a regular file at the output.
* `encode`: `Mp4Writer`, one video stream from planar YUV (4:2:0, 4:2:2
  or 4:4:4 at 8 or 10 bits, tagged with any colour, optionally behind an
  edit list, with its own timestamps or cropped on the left, which
  FFmpeg's `h264_metadata` or `hevc_metadata` filter writes into the
  SPS, `ffi::CropFilter`) and optional planar-f32 audio,
  written atomically like `inject`, which also means it refuses an
  output that isn't a regular file; and `remux_to_matroska`, a stream
  copy into Matroska. The synthetic media generator uses them today;
  M2's export builds on the writer.
* `decode`: `VideoDecoder`, which decodes one video stream in order, in
  software or on a `HwDevice` (single-threaded, with spare surfaces,
  `open_hardware`), and hands out frames in system memory, downloaded
  into its pool. It
  reads only its own stream (the others are discarded), holds back a
  packet flagged corrupt until the next read, and ends a damaged file with
  an error after the frames it could decode (D13). `open_cancellable`
  stops reading when a flag is set, and `skipped_packets` counts packets
  skipped as invalid data. On a device it opens a decoder that can use the
  device and never falls back to software (D3, D14); any error there that
  isn't a refusal is `HwFailed`, never damage (D30). For `seek` it also
  seeks (clearing a read failure, not a refusal, D29), says where a seek
  landed (and the pts of a keyframe landed on) and the last packet read
  (dts, else pts), the least lag of a keyframe's pts behind its dts, the
  last keyframe the file holds when its index lists more than the file
  does (an MP4 cut short), and as soon as reading has failed (the
  frames still to come out may follow frames the damage lost), reads a
  stream's first packets' timestamps and seeks back, stops at packet and
  frame boundaries on a `StopToken`, and skips non-reference frames until
  a packet at a given pts, counting the packets sent meanwhile.
* `backend`: the traits everything above media opens media through (D29):
  `MediaBackend` (`probe`, `open_video`, `open_audio`) and `VideoDecode`
  (`seek`, `next_frame`, `catch_up`, events and stats), with
  `VideoRequest`, `SeekMode` and `SeekOutcome` (the frame, its
  `Exactness` and the `SeekCost`), `VideoStreamInfo` (origin, timing,
  frame count, layout, colour, sphere mapping) and `VideoFrame`. A
  `VideoFrame` holds an FFmpeg frame or planes made without FFmpeg,
  implements core's `VideoPlanes` without copying, shares its layout
  (`Arc<PixelLayout>`), counts the buffers it keeps (`bytes()`, by
  `AVBufferRef` size, for the cache's budget), and carries its `run`
  (begun at a demuxer seek, or at the first frame after catching up
  skipped frames) and `seq` (its place in decode-output order in the
  run, which every frame decoded takes), so that frames of one run with
  consecutive `seq` are known to be adjacent. `FfmpegBackend` opens a
  confined decoder in software or, for `DecodeMode::Hardware`, on VA-API
  or Vulkan Video: on the request's device, or on the backend's own of
  that kind, opened once on its render node the first time a decoder
  asks and shared, as is the error when it can't be (D30). Audio (slice
  6) returns `MediaError::NotYet`. With the `fault-injection` feature,
  which only synth's tests turn on, a request can fail a frame as the
  device would. The types that cross threads are checked `Send`, and
  `Sync` where they are shared, at compile time.
* `seek`: `FfmpegVideo`, the `VideoDecode` on `VideoDecoder`, with the
  frame-exact seek of plan section 3.3 (D29), which drops the leading
  pictures of the keyframe a seek lands on, frame-index addressing
  (`frame_address`, `seek_frame`) and the stream's timing facts (constant
  rate from `stts`, `ctts` and an edit list that shows the track once, or
  from a snapped Matroska default duration that the first 64 blocks and
  the Cues keep to; the end of the stream from the container, else the
  last frame's interval; the frame count from the MP4 index; the mapping
  from the track's spherical boxes). Streams whose timestamps tie or go
  backwards seek by the tie rule, the latter by decoding from the start.
  It converts frames of a format that isn't uploaded as it is through
  swscale, once announced with `DecoderEvent::Converted` (D30). On a
  device, frames stay there until handed out: a seek holds its best
  candidate as a device frame and downloads only the one it returns.
  `fell_back` has a software decoder take over a failed hardware one's
  place, so that it goes on from the frame after the last one returned.
* `fallback`: `Fallback::Software` (D30). A device that can't be opened,
  turns the stream down or fails on its first frame has the stream
  opened in software; one that fails later has it reopened in software
  (`FallbackVideo`), once, with a seek it failed in done again. Either
  way a `tracing::warn!` and `DecoderEvent::FellBack` say why, and every
  frame after is `DecodePath::FellBack`. With `Fallback::Fail` the error
  is returned.
* `hw`: which render node hardware decoding runs on, the GPU's that
  renders: by the PCI bus id wgpu gives its adapter, else by its vendor
  and device ids, else `/dev/dri/renderD128`, over a sysfs root that
  tests fake; `--render-node` overrides it (D30).
* `color`: `resolve`, the colour default rules (D30): a frame's codes,
  as FFmpeg's ints, become the `ColorInfo` the renderer uses, with an
  `assumed` flag for each field a rule filled in (siting only where the
  chroma is subsampled).
* `detect`: camera and layout guesses from container structure, each with
  its evidence.
* `doctor`: the capability checks and the fixes (D18). A failed check
  names its `fault`: this FFmpeg build (`ffmpeg`), the driver refusing or
  failing (`driver`), or the driver giving a wrong picture or stream
  (`wrong_output`). The embedded one-frame bitstreams are in
  `crates/media/fixtures/`, whose README gives the commands that
  regenerate them byte for byte.

`MediaError` has twelve variants: `Io`, `NotAFile`,
`UnsupportedContainer { path, format, description }` (FFmpeg would read
the file with a demuxer outside `DEMUXERS`; the message names it and the
containers Spherecut opens), `Open`, `NestedOpenRefused { path, url }`
(FFmpeg tried to open something else while opening the file, which
`io_open` refused), `Decode`, `PictureTooLarge` (over `MAX_PIXELS`),
`HwUnsupported { path, kind, refusal: HwRefusal }` (the device can't decode
this stream; software may), `HwFailed { path, kind, reason }` (decoding
or downloading a frame on the device failed; software may still decode
it) and `Hardware { kind, reason: HwError }` (the device can't be
opened), `UnsupportedFormat { path, pix_fmt }` (frames
in a pixel format swscale can't convert) and `NotYet { what }` (a part of
the backend a later M1 slice brings). `code()` gives each a stable snake_case code for
the JSON contract, and `MediaError::CODES` lists them (D27). `VideoDecoder::open` returns `HwUnsupported`
(`NotOffered`) when no decoder for the codec can use the device, and
`next_frame` returns it when the device turns the stream down. A software
frame from a decoder that never asked for the device's format, or that
turned it down, is `HwUnsupported` too, not a `Decode` error. Once
`next_frame` has returned it, or `HwFailed`, every later call returns it
again and decodes nothing. Messages that include their cause don't also return it
from `source()`, so `{e:#}` prints each cause once.

`spherecut_media::init()` initialises FFmpeg once and installs the log
bridge. FFmpeg's own log level follows what the `tracing` subscriber
accepts for target `ffmpeg`, so a subscriber installed after `init()`
should call `sync_ffmpeg_log_level()`.

## The CLI's JSON contract

`spherecut_cli::contract` builds what `--json` prints (D27, and
`docs/cli-json.md` field by field): one envelope per run, `{"schema",
"ok", "result"?, "error"?, "warnings"}`, with `schema` such as
`spherecut.probe/1`. probe's result is `{"files": [...]}`, one
`ProbeFile` per path, and doctor's is its `DoctorReport`. `Exit` holds
the exit statuses (0 to 2 used, 3 and 4 reserved for `frame` and the
GUI) and `codes` the CLI's own codes; `all_codes()` adds `MediaError`'s
and probe's. `main.rs` parses with clap's `try_parse_from`, so that a
command line that doesn't parse is a document too under `--json`
(`wants_json` looks for `--json` before any `--`), and writes the
document with `Envelope::to_line()` after the run. The module needs
only serde, serde_json, core and media. A1 moves it into
`spherecut-agent` (or a crate below both), which the CLI will depend on
and which can't depend on the CLI, and the CLI re-exports it (D27).
Under `--json`, a panic hook writes an `internal` document in the run's
schema (`schema_for`) unless the document has started, and `main` exits
1 after any panic, parsing included; a flag keeps the run to one
document. Debug builds panic on `SPHERECUT_DEBUG_PANIC` (`main`,
`thread` or `usage`) for the contract test. A1's `spherecut call` and
`spherecut mcp` are to reuse the module.

## Coordinates and conventions

* World and camera space are right-handed: **+X right, +Y up, −Z forward**.
  The centre of an equirect frame is forward.
* Longitude `atan2(x, −z)`, latitude `asin(y)` (the shader takes it as
  `atan2(y, |xz|)`, the same angle, for its precision at the poles).
  Equirect `u = 0.5 + lon/2π`, `v = 0.5 − lat/π`, `v = 0` at the top.
* Normalised image coordinates `(u, v) ∈ [0, 1]²`; texel centres at
  half-integers.
* Stored rotations are unit quaternions (`glam::DQuat` on the CPU). A view
  orientation `q` maps view-space rays to world space: `world = q * view`.
  The UI may show yaw/pitch/roll, derived on display.
* A lens or virtual camera looks down its own −Z axis.
* Yaw, pitch and roll (`sphere::orient::Ypr`): yaw > 0 turns right, pitch
  > 0 looks up, roll > 0 rolls the camera clockwise, and
  `q = R_y(−yaw) · R_x(pitch) · R_z(−roll)`.
* An output pixel `(i, j)` has its centre at `(i + 0.5, j + 0.5)`, from the
  top-left corner. `sphere::pixel::to_lens` maps a point of a `w` × `h`
  view to the lens's `p = (2x/w − 1, (1 − 2y/h) · h/w)`, so `p.x` runs from
  −1 to 1 across the width and `p.y` up.

## Time

All stored time is exact (`spherecut_core::time`):

* `Rational`: reduced `i64/i64`, checked arithmetic in `i128`.
* `Time`: seconds as a `Rational`; timeline positions are `Time`.
* `FrameRate`: positive `Rational` (`30000/1001` for 29.97).
* `FrameTime`: a `Time` that is a frame of a video stream, in clip time
  (0 at the stream's first presented frame), with the frame's index from
  that frame when it is known (constant frame rate, or counted).
  `FrameTime::at(rate, n)` is n / rate.
* Streams keep integer PTS in their own timebase; conversion to and from
  `Time` is explicit about rounding. Variable-frame-rate sources are
  addressed by PTS, never by frame index.
* A decoded stream has two axes (D29). Its pts axis is the stream's own
  ticks of `time_base`; `origin_pts` is the pts of its first presented
  frame, whatever an edit list or B-frame delay does to it. Clip time is
  `(pts − origin_pts) × time_base`, so clip time 0 is that frame. A seek
  to clip time `at` looks for the frame with the greatest pts at or
  before `origin_pts + ⌊at / time_base⌋`, and frame N of a constant-rate
  stream is found at `(N + ½)` frame durations, which survives a
  container that rounds timestamps (Matroska's milliseconds).
* Serialized (D27), a `Rational` or `FrameRate` is the string `"n/d"`
  (`"n"` for an integer), which `Rational` also parses with `FromStr`; a
  `Time` is `{"exact": "n/d", "seconds": float}`, and a `FrameTime` adds
  `"frame"` when it has an index (a null one reads as none). Reading
  ignores `seconds`, so every one reads back exactly. probe's container
  times are FFmpeg's container timestamps, not clip time
  (`docs/cli-json.md`, "Two timelines").

## Rendering

Built in M1 slice 3a for equirect sources (`spherecut-render`, DECISIONS
D31); stitching and stabilization join the same pass later.

```
VideoPlanes ──Uploader──▶ GpuFrame (slot) ──Projector──▶ LinearTarget ──Encoder──▶ screen or Rgba8Unorm
 (CPU planes)  write_texture   canonical planes   project.wgsl    Rgba16Float,       encode.wgsl
               or belt,        + mips, Source     ray, LOD,       linear light       (sRGB OETF)
               ingest, mips    uniform            sample, colour                     │
                                                                                     ▼
                                                     render_still: readback ──▶ Image (RGB 8/16-bit, PNG)
```

* **`Gpu`** is one wgpu device on Vulkan, chosen by `AdapterChoice`
  (`Auto`, `Cpu` for lavapipe, or a name); `Gpu::headless` makes one for
  `frame`, tests and benchmarks, and `Gpu::from_parts` wraps eframe's.
  `GpuCaps` says what it can do (timestamps, anisotropy, its largest
  texture). All device creation is in one function that slice 5b
  replaces to add Vulkan Video and DMA-BUF import.
* **Slots** (`GpuFrame`, for a `SlotShape`) hold a frame's planes as
  canonical textures: `R8Unorm`/`Rg8Unorm` for 8-bit planes, and
  `R16Float`/`Rg16Float` holding code / (2^b − 1) for deeper ones, each
  with an area-weighted mip chain capped near 64 pixels, plus the
  frame's `Source` uniform: its colour matrix, chroma siting and where it
  sits on the sphere (`Placement`: sv3d crop, stereo eye).
* **`Uploader::upload`** copies a frame's planes into a slot
  (`write_texture`, or a staging belt), runs the ingest pass that turns
  deep integer planes into f16 and the mip passes, and submits once,
  never waiting. A frame already on the GPU can fill level 0 itself,
  then call `Uploader::record_mips` and `GpuFrame::set_source`.
* **`Projector::record`** draws a slot through a `ViewParams`
  (orientation and `Lens`) into a `LinearTarget`: per pixel a ray from
  the lens, rotated into the world, its equirect uv, an LOD from central
  differences of the direction, a trilinear (or anisotropic) tap of each
  plane, and the 3×4 colour matrix and the sRGB EOTF. Pipelines exist per
  `Filter` and `DebugMode` and are built on first use.
* **`Encoder::draw`** writes a linear target's pixels one to one into a
  render pass's attachment with the sRGB OETF, at an origin, for egui's
  frame (slice 7) or an offscreen texture.
* **`render_still`** (and `StillRenderer`, which keeps its pipelines)
  does all of it headless and reads back an `Image`: 8-bit through the
  encode pass, 16-bit or linear from the linear target.
* **`reference`** is the same projection in f64 on the CPU, with the
  GPU's storage formats emulated; the Tier 1 tests compare the two on
  lavapipe.

What is still planned, for every source type: in WGSL, "direction on the
unit sphere → texel(s)" for the GoPro EAC pair and calibrated dual
fisheye, with the view orientation composed with the stabilization
rotation, lenses blended across the seam, and the colour pipeline in
float before the output. Stitching, stabilization and reframing then
happen in one pass shared by the viewer and the exporter, and an
intermediate equirect exists only as the output of a 360 export.

## Threads *(planned, M1)*

* UI thread: egui, never blocks on decode, disk or GPU readback.
* Decode workers per open source, feeding a bounded frame cache.
* Audio thread (cpal): owns the playback clock; video presentation follows
  it.
* Background workers for thumbnails, waveforms, proxies and export, each
  with progress reporting and cancellation tokens.
* Cancellation (built): `core::cancel`'s `StopSource` hands out
  `StopToken`s by generation; starting a new generation stops the older
  tokens, as a new seek stops the last one, and shutdown stops them all
  and raises the flag a decoder's FFmpeg interrupt reads (D13, D29).

## Testing tiers

* **Tier 1** runs anywhere: unit and property tests, software decode and
  encode, synthetic media that the tests generate themselves (among them
  the seek oracle, `crates/synth/tests/seek.rs`), doctor run
  against lavapipe, `VideoDecoder` turning streams down on lavapipe's
  Vulkan device, and the fallback to software on lavapipe, on a render
  node that isn't there and, with media's `fault-injection` feature, at
  every frame of a software stand-in for the device
  (`crates/synth/tests/fallback.rs`). The Tier 2 backend tests check
  their own checks the same way. The renderer's tests run on lavapipe
  (`crates/render/tests/`): every pipeline, the uniform layouts, the
  mips, both upload paths, and `render_still` against the CPU reference;
  golden images come in slice 3b.
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
