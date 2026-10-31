# Spherecut architecture

This describes the design as built, plus the planned parts marked
*(planned)*. DECISIONS.md records why each choice was made.

## Crates

```
          app (eframe viewer; M1 slice 7)   cli (`spherecut` binary)   tests
                          \                |                    /
                           +---- export (M2.4 and M2.5) -------+
                                      |
   playback (Player, decode workers, frame cache, GPU slots, presenter, the file's audio; M1 slices 5 and 6)
                     |                                    |
   render (wgpu on Vulkan, WGSL; M1 slice 3)   audio (cpal output, chunk ring, audio clock; M1 slice 6)
                                      |
             motion (IMU, stabilization)   stitch (lens models, blending)  (planned)
                                      |
                       camera (insta360, gopro, dji)  (planned)
                                      |
     media (FFmpeg, MediaBackend and frame-exact seek, probing, MP4 boxes, doctor)
                                      |
   sphere (projection math)      core (time, frames, colour, stop tokens, clocks, the UI thread's guard, atomic writes, the project model)
```

`synth` (test patterns and media, and backends that stand in for FFmpeg)
and `testkit` (test helpers) sit beside this stack and are used only by
tests and `xtask`.

core depends on sphere (since M2.1, D48), whose projections and
quaternions the project model holds; sphere depends on neither.

agent depends on export (since M2.5): the JSON contract it holds for
the CLI lists the export's codes and turns its errors and fallbacks
into `render`'s documents (D51), and A2's tools will drive the same
job handle.

render depends on core and sphere only, not on media: frames reach it
through `core::video::VideoPlanes`, so the library has no FFmpeg
dependency, and its tests, the goldens and M3's stitcher make the frames
they feed it without decoding. Its test binaries do link FFmpeg, through
synth, which depends on media. audio depends on core alone: the decoded
samples reach it from playback's `audio-decode` thread.

| Crate | Role | Status |
|---|---|---|
| `spherecut-core` | Product name constants (`brand`), exact rational time and its serialized form (D27), the `--time` grammar (`TimeSpec`), what a decoded frame is (`video`: pixel layouts, planes, colour), stop tokens by generation (`cancel`), the clocks playback runs on (`clock`: the `Clock` trait, `SystemClock`, the frame-steppable `ManualClock`), the guard that a blocking call never runs on the UI thread (`blocking`: `mark_ui_thread`, `may_block`, D34) files written whole or not at all (`fs::TempFile`, D16), and the project model (`project`: media, a sequence of clips with trims, view keys and markers, look-at targets, every change an `Edit` through `Project::apply`, undo `History`, the JSON project file; D47, D48; and `project::anim`, the view of a clip at a time from its keys, D50) | M0: name and time; M1: `video`, `cancel`; slice 4: `TimeSpec`, `fs`; slice 5: `clock`, `blocking`; M2.1: `project`; M2.2: `project::anim` |
| `spherecut-sphere` | CPU reference projection math in f64: equirect and its uv gradient, cube/EAC, fisheye lens models, view projections and morphs and their inverse (`project_morph`), output pixels to lens coordinates (`pixel`), yaw, pitch and roll (`orient`), rotation helpers, keyed lens shapes (`LensShape`), and the keyframe splines (`spline`: the scalar spline with eases and spin, Free's SO(3) spline, `follow_spin`'s maths) | M0; M1 slice 3a: `pixel`, `orient`, `uv_gradient`, `project_morph`; M2.2: `spline`, `LensShape` |
| `spherecut-media` | FFmpeg in a safe RAII layer, probing with structural damage checks, our own ISO-BMFF box reader and metadata injector, a sequential video decoder, the `MediaBackend` traits with frame-exact seek in software and on VA-API or Vulkan Video with fallback to software, the render node lookup, the colour default rules, audio decoding to the output's rate and channels on the clip's time axis (`audio`, D35), a small MP4 writer, `doctor`'s capability checks | M0; `MediaBackend` from M1 slice 2a, hardware from 2b, audio from 6 |
| `spherecut-synth` | Synthetic scenes (grid, high-frequency, seam), YUV conversion at any matrix, range and subsampling, the supersampled ideal of a view (`render_view_ideal`, which the goldens hold minified views to), tones and first-order ambiX, test clips (open GOPs, edit lists, VFR, Matroska remuxes, seeded noise for camera bitrates) and broken files, `SyntheticBackend` and `GatedBackend` | M0; M1 slices 2a, 2b and 3b |
| `spherecut-testkit` | Tier 2 gating (`tier2::require_*`, `require_audio_device` among them, the `TIER2-SKIP:` marker), benchmark recording (`bench`, with latency percentiles) and FNV-1a hashing (`fnv64`) | M0; M1 slice 3a: `fnv64` (and `compare` until M2.4) |
| `spherecut-camera` | Per-camera raw formats: file pairing, vendor metadata, calibration | M3 |
| `spherecut-stitch` | Lens calibration models, seam blending, exposure matching | M3 |
| `spherecut-motion` | IMU ingest, time sync, orientation fusion, stabilization | M4 |
| `spherecut-render` | The wgpu device (Vulkan only), canonical plane textures with area mip chains, the upload path, the projection and encode shaders with colour from `ColorInfo`, headless stills and readback, and the f64 CPU reference the tests hold the GPU to (D31), with the golden images in its tests (D32); for export, equirect output, the output frame, the YUV pass for the encoders, motion blur and a frame's views from the clip's keys (`output`, D49); image comparison (`compare`: PSNR, SSIM, per-pixel difference statistics), from testkit so that agent can use it (amendment 2 item 32); later scopes | M1 slices 3a and 3b; M2.3; M2.4: `compare` |
| `spherecut-playback` | The headless playback engine (D34), with no UI type: the `Player` the GUI's viewer, the benchmarks and M2's export drive (commands that never wait, `present` once a UI frame, `snapshot`, `events`, shutdown); a decode worker per video stream, on a decoder chosen from doctor's checks (`decoder`), with seeks answered from the cache's runs and catch-up; the bounded frame cache (`FrameCache`: decoded frames linked by run, the window ahead of the playhead, eviction and back-pressure) and its budget (`budget`: from `/proc/meminfo` and cgroup v2 limits, or a size given); the GPU slots (`SlotRing`) the upload thread fills and the presenter shows from; the file's audio (`audio`: `DeviceAudio`, which opens the output and the `audio-decode` thread, and the preroll that waits for audio, D35); and the numbers the overlay, the smoke run and the benchmarks read (`PerfStats`) | M1 slices 5 and 6 |
| `spherecut-audio` | The audio output (D35): the default device through cpal (PulseAudio's protocol, else ALSA) on the `audio-out` thread, the null output where there is none, the fake device the A/V tests script; the lock-free chunk ring the decoder fills; the callback all three run; and `AudioClock`, the clock video follows. Later mixing, ambisonics and loudness | M1 slice 6; M6 |
| `spherecut-export` | The export (D52): `Export` runs an `ExportJob`, a project's sequence, on three threads (decode, render through the YUV pass, encode and mux) into a locked temporary file, then one last pass (V2, `SA3D`, the V1 box, the moov first) and section 9's read-back checks before it is put in place; the presets and their bitrates, the software encoders (libx264, libx265, libsvtav1) under a VBV cap, the size estimate and free-space check, audio copied (AAC and PCM cut to the sample), AAC or none; the job handle (progress, cancel, wait, the `Report` with each stage's time) that cli, the app and A2 drive; the VA-API encoders (VBR under the same cap, Mesa's quality level) tried before software, with a restart in software when one fails in its first second, and the decode on VA-API going on in software; the pace an export from the app keeps while the UI is busy (`Pace`, one frame per UI pass) | M2.4; M2.5: VA-API, the order and fallback, the decode path, stage times; M2.7: `Pace` |
| `spherecut-app` | The GUI (D33, D36): eframe on render's device; the tiles (the viewer beside two rows of tabs: Info, Inspector, Doctor and Files, then Curves and Sphere); the viewer's paint callback, one pixel per pixel; the drag, zoom and projection switch (`view`); transport and scrubbing; the doctor check on a worker and its banners; the player's notices (`notices`); the overlay (`overlay`), the settings and their window (`settings`), the raw view, the smoke run (`smoke`) and the toolkit check's timeline prototype (`proto`, D53); `run`, which the CLI calls with no command; the reframer (M2.6, D54): the open project and its undo history (`doc`), the file work as jobs, the keyed view and look-around and the output frame (`reframe`), and the timeline strip (`timeline`); and (M2.7, D55) the curve editor (`curves`), the sphere overview (`overview`), the Inspector and look-at targets (`inspector`) and the export window with its job (`export`). `SpherecutApp::ui` takes an egui `Ui` and no eframe type, so kittest drives what eframe runs | M1 slices 7 and 8, M2.6, M2.7 |
| `spherecut-agent` | Agent integration (D23, D43 to D46): the MCP server (`rpc`), the tools and their hand-written schemas (`tools`, `ops`), the roots (`roots`), the JSON contract of `--json` (`contract`, D27), frame's core that look renders through (`still`), look's canvas and font (`image`, `font`), the instructions and the skill; later the GUI attach bridge | A1 |
| `spherecut-cli` | The `spherecut` binary: the GUI with no command (slice 7), `probe`, `doctor`, `frame` (and `frame --project`, a sequence's frame as the export renders it), `render` (a project's export, one envelope a line, D51), `mcp`, `call` and `agents install-skill` (A1). Its library half re-exports `spherecut_agent::contract` as `contract` | M0: `probe`, `doctor`; M1: the JSON contract; slice 4: `frame`; slice 7: the GUI; A1: `mcp`, `call`, `agents install-skill`; M2.5: `render`, `frame --project` |
| `xtask` | `check`, `compat` (clippy and a link of the tests against other FFmpeg builds, and Tier 1 against one, D2), `gen-media`, `hw-test` | M0; `compat` from 2026-10-24 |

Crates are created when they get real content, not as empty shells. The
planned ones above don't exist yet. Every crate root forbids `unsafe`
code except media's and the `spherecut` binary's, which deny it and allow
it only in named FFI modules (media's `ffi` and `hwprobe`; the binary's
`interrupt`, frame's SIGINT handler, from slice 4),
and render's, which allows it in `dmabuf` and `vkshare` alone (slice
5b's zero copy, DECISIONS D28).

## Media crate (as built in M0, with M1's confinement of FFmpeg and `MediaBackend`)

`unsafe` code lives in two modules, each block with a SAFETY comment:

* `ffi.rs` holds what ffmpeg-next doesn't cover safely: codec-parameter
  fields (colour, channel layout, display matrix, profile, extradata),
  stream media types, hardware device types and codec hardware configs,
  `HwDevice` (a reference-counted device context, released on drop) and
  `HwError`, the `get_format` callback that refuses software fallback,
  opening files with FFmpeg confined to them, reading metadata, finding a
  stream's decoder, opening decoders under a picture size limit and a video
  decoder by its int codec id, reading a frame's format, planes,
  timestamps, flags and colour codes as ints (`frame_fields`), the
  layout a pixel format is uploaded in (`pixel_layout`, from
  `AVPixFmtDescriptor`) and the format swscale converts the others to
  (`conversion_target`, `Scaler`), copying hardware frames to system
  memory (`download_into_pool`, into a `DownloadPool` of buffers mapped on
  huge pages for Vulkan and from `av_buffer_alloc` for VA-API, and
  doctor's `download_frame`) and the surfaces a hardware decoder keeps spare
  (`set_extra_hw_frames`), `av_seek_frame` and the demuxer index (its end, its first,
  last and shown entries, its timestamps, the keyframe before a time),
  skipping non-reference frames, the bytes a frame's buffers take
  (`buffer_bytes`), setting other streams to `AVDISCARD_ALL`, an audio
  decoder opened by its int codec id with its sample format and channel
  layout read as ints (`open_audio_decoder`, `audio_frame_fields`), the
  RAII `Resampler` (`swr_alloc_set_opts2`, `swr_init`, `swr_convert`)
  to packed f32 (D35), library versions, and the
  bridge that sends FFmpeg's `av_log` to `tracing` under the target
  `ffmpeg`. Its child `ffi::vulkan` (slice 5b's path B) makes FFmpeg's
  Vulkan device context on the renderer's own device from its raw
  handles and queues (`HwDevice::vulkan_shared`), and gives a Vulkan
  Video frame's images and their state under FFmpeg's frame lock
  (`VkFrame`, core's `VkPicture`). C enums are never made into bindgen's
  Rust enums (DECISIONS D11). In more detail:
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
    planes on 256-byte boundaries, so render's staging belt copies a
    plane in one go (`write_texture` does where the row is a multiple of
    256 already, or the device's `optimalBufferCopyRowPitchAlignment` is
    256, and otherwise copies row by row; D31). The buffer returns to
    the pool when the frame's last reference drops, and a new format or
    size rebuilds the pool.
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
* `nal`: whether an H.264 or HEVC packet, length-prefixed as its `avcC`
  or `hvcC` says, holds a picture FFmpeg's decoder flags a keyframe: an
  IDR picture or a reference picture whose recovery point counts no
  frames, or an IRAP picture of HEVC's base layer (D29).
* `spherical`: Spherical Video V1 and V2 and SA3D, read from the boxes and
  written as box bytes.
* `inject`: adds or replaces boxes in the first video and audio sample
  entries, fixing sizes and chunk offsets, written atomically through
  `core::fs::TempFile`'s `persist_replace` (D16). It
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
  an error after the frames it could decode (D13). A cut is a file that
  ends before what its index lists for the stream (MP4), or before the
  end its Matroska Segment's size gives. `open_cancellable`
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
  a packet at a given pts, counting the packets sent meanwhile. A decode
  from the stream's first packet, once packets were sent, runs on a
  decoder opened afresh, unless the first packet's NAL units (`nal`) or
  FFmpeg's flag on the first frame say it starts at a keyframe; a reopen
  that fails is `HwFailed` on a device, and otherwise an error until a
  seek (D29). It says where its last error came out: the last packet
  sent, and the errors since.
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
  asks and shared, as is the error when it can't be (D30). `open_audio`
  opens `audio`'s `FfmpegAudio` (below). With the `fault-injection` feature,
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
  Playing on, `next_frame` says a failed read once, as a `Damaged` event
  with its cause, before the first frame the decoder drains after it or
  before the error (D13). Any damage is said once from one seek to the
  next, however often decoding meets it (D29).
  It converts frames of a format that isn't uploaded as it is through
  swscale, once announced with `DecoderEvent::Converted` (D30). On a
  device, frames stay there until handed out: a seek holds its best
  candidate as a device frame and downloads only the one it returns.
  `fell_back` has a software decoder take over a failed hardware one's
  place, so that it goes on from the frame after the last one returned.
* `fallback`: `Fallback::Software` (D30). A device that can't be opened,
  turns the stream down or fails on its first frame has the stream
  opened in software; one that fails later has it reopened in software
  (`FallbackVideo`), once, with a seek it failed in done again, which
  doesn't say again the damage the device's attempt said. Either
  way a `tracing::warn!` and `DecoderEvent::FellBack` say why, and every
  frame after is `DecodePath::FellBack`. With `Fallback::Fail` the error
  is returned.
* `audio`: `FfmpegAudio`, the `AudioDecode` behind `open_audio` (D35).
  Its own confined demuxer and decoder hand out packed f32 at the
  output's rate and channels in chunks on the clip's axis (sample `s` is
  clip time `s / rate`, 0 being the video's first frame): AAC's priming
  and the last frame's padding gone, edit lists applied, audio before 0
  dropped and audio after it led by silence. Mono is duplicated, stereo
  passed through, first-order ambiX decoded to two cardioids, the rest
  mixed down by swresample, and stereo spread to the output's channels.
  The resampler starts only on a grid of input samples whose phase a
  decode from the start has, so a seek, which decodes from 100 ms
  before its time and drops what comes before round(at × rate), gives a
  sequential decode's samples. A coarse time base (Matroska's
  milliseconds) is snapped to where the frames' sizes put them.
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

`spherecut_agent::contract`, re-exported as `spherecut_cli::contract`
since A1.4 moved it below the CLI, builds what `--json` prints (D27, and
`docs/cli-json.md` field by field): one envelope per run, `{"schema",
"ok", "result"?, "error"?, "warnings"}`, with `schema` such as
`spherecut.probe/1`. probe's result is `{"files": [...]}`, one
`ProbeFile` per path, doctor's is its `DoctorReport`, and frame's is a
`FrameResult` (`spherecut.frame/1`), present only when the PNG was
written. `Exit` holds the exit statuses: probe and doctor use 0 to 2,
and frame also 3 (a device or codec it needs is missing) and 4
(cancelled), mapped from its error's code by `frame_exit`. `codes` holds
the CLI's own codes and `codes::warnings` frame's warnings; `all_codes()`
adds `MediaError`'s, probe's and `RenderError`'s. `main.rs` parses with clap's `try_parse_from`, so that a
command line that doesn't parse is a document too under `--json`
(`wants_json` looks for `--json` before any `--`), and writes the
document with `Envelope::to_line()` after the run. The module needs
only serde, serde_json, core and media.
Under `--json`, a panic hook writes an `internal` document in the run's
schema (`schema_for`) unless the document has started, and `main` exits
1 after any panic, parsing included; a flag keeps the run to one
document. Debug builds panic on `SPHERECUT_DEBUG_PANIC` (`main`,
`thread` or `usage`) for the contract test. `spherecut call` prints its
`spherecut.call/1` envelope through the module too, and the agent's
tools reuse its codes.

## `spherecut frame`

`crates/cli/src/frame.rs` renders one frame of a stitched equirect into
a PNG (M1 plan section 5, D27), in the plan's order. Steps 4 to 8 are
`spherecut_agent::still`'s since A1.4, so that the agent's `look` renders
its tiles the way frame does and frame's tests cover that path; frame
keeps its flags, its output file, SIGINT and its JSON:

1. clap parses the flags, `--time` through `core::TimeSpec` (exact
   decimals, `m:s` and `h:m:s`, rationals, `Nf`), and
   `FrameArgs::check` refuses what clap can't (a `--fov` past the
   projection's limit, `--reference` with `--filter aniso`): `usage`,
   exit 2;
2. the output is checked before any work (`check_output`): something
   there is `output_exists` without `--overwrite`, and a directory, pipe
   or device, or a name ending in `/` or `/.`, is `not_a_file` either
   way;
3. `cli::interrupt` installs the SIGINT handler, then a
   `core::fs::TempFile` is created next to the output (D16), so a first
   Ctrl+C always finds the handler there to drop it; the rest runs on a
   worker thread named `frame` while the main thread polls the handler's
   flag every 10 ms (below);
4. with `--project` (M2.5, D51), the project and its media load as
   `render` loads them, and the sequence's frame at the time asked for
   (sequence time) gives the file, its stream, the source time to seek
   to, the output's size and the frame's views (`render::output::
   frame_views`, with the sequence's motion blur), and the filter is the
   export's; then probe, through the confinement (D24), picks the stream
   (`--stream`, else the first video stream that isn't a cover picture)
   and refuses a file not tagged as an equirect (`not_equirect`, naming
   what it is) unless `--equirect` (which a project's clip implies);
   probe's damage becomes warnings;
5. `Gpu::headless` takes the adapter `--adapter` (or
   `SPHERECUT_ADAPTER`) names and checks `--size` against its limit,
   unless `--reference` renders on the CPU;
6. the decoder opens on `media::hw::render_node` for that GPU's ids
   (D30), with `--decoder` choosing the mode (D39: `auto` is VA-API with
   `Fallback::Software`, or software at once when the node isn't there);
7. an `Exact` seek, or for `--frame N` on a variable-rate stream a count
   from the first frame; `AfterLast`, or a target the stream's time base
   can't hold, is `out_of_range`, damage before the frame (the count's
   included) fails by its cause and damage after it is a warning; for
   `--frame N` on a constant-rate stream the frame shown must be frame N
   (`check_index`), else frames are missing there (`damaged`, or
   `out_of_range` when it is the last frame);
8. `render_still` through the views (several, averaged, for motion
   blur), or for `--reference` the f64 CPU reference
   (`reference::render`, trilinear as `render_trilinear` is, with the
   file's placement) rounded to the PNG's codes;
9. the PNG goes into the temporary file, which `persist_new` publishes
   (`persist_replace` with `--overwrite`);
10. one line on stdout, or the `--json` document with the `FrameResult`.

**SIGINT.** `cli::interrupt` is the binary's one FFI module (D28): it
asks for SIGINT's current action, and unless that is `SIG_IGN` (kept,
as for a script's background job) installs a handler with
`SA_RESETHAND | SA_RESTART` that only stores `true` in an atomic. The main thread, polling it, calls
`StopSource::shutdown()` on the first Ctrl+C, which stops the seek
between frames and raises the flag the decoder's FFmpeg interrupt
callback reads inside a read (`VideoRequest::interrupt`). The other
steps take no stop token: the worker checks between steps, so a Ctrl+C
during the render or the PNG's encode takes effect when it ends.
Whatever the worker returns after that, the run ends `cancelled` (exit 4) and the
temporary file is dropped. The handler is reset as it runs, so a second
Ctrl+C kills the process. probe and doctor never install it.

## The agent server *(A1, D43 to D46)*

`spherecut mcp` serves the tools of `crates/agent` over MCP on stdio;
`spherecut call <tool> '<json>'` runs one of them the same way from a
shell. The crate has no `unsafe`.

- **`rpc`**: the JSON-RPC reader, the session (revisions, lifecycle),
  and a FIFO of calls for two workers, each call with a `CallCx` (its
  deadline, 50 s; its stop token, which FFmpeg's interrupt callback
  sees; progress). A reaper answers `timed_out` for a call stuck 2 s
  past its deadline and replaces its worker (D44).
- **`tools`**: the tool list (each schema hand-written in
  `docs/agents/schemas/`, `include_str!`ed, checked by `schema`'s
  validator before an op runs) and the dispatch to `ops`. A result is
  its JSON plus any images; `text::sanitize` goes over every result.
- **`roots`**: every path an agent names is opened with `openat2`
  beneath a read or write root, and media reads the descriptor through
  `/proc/self/fd` (D45).
- **`ops`**: `probe` (summaries, folders a page at a time), `look`,
  `analyze`, `check` and `doctor`. look opens the file through `still` (frame's core: probe,
  the decoder D39 picks, the seek and the render, shared with `spherecut
  frame`), decodes each time once in order, renders every tile of that
  time from the frame on the one GPU a process keeps (made on the first
  call, made again if lost), draws the tiles and their labels on an RGB
  canvas (`image`, with the 8×8 public-domain font in `font`), and
  encodes it as JPEG (`media::encode_jpeg`) or PNG within the image
  budget. doctor with a file opens it the same way and reports the
  decode path look gets. analyze (D46) cuts a stretch into buckets
  whose edges are fractions: `timing` reads the stream's packets
  (`media::packets`) without decoding, `motion` decodes through `still`
  as look does and compares each frame's 64×32 luma with the last, and
  `audio` decodes every channel at the stream's rate
  (`FfmpegAudio::open_raw`) on the video's clip time. Each stops at a
  bucket's end before the deadline and says where to go on from. check
  (D46) reads only probe's report: rules that always run (nothing
  damaged, the projection fits one eye's picture, V1 and V2 agree, SA3D
  fits its track) and a target's (YouTube 360, from its help pages as
  fetched on 2026-10-30; flat 16:9 or 9:16; archive), each with its fix,
  under 2 KiB.
- **`contract`**: the CLI's JSON contract (D27), here since A1.4 so that
  `still` can return its errors and the CLI re-exports it.
- **Teaching** (A1.7): `initialize` gives the instructions (the
  conventions and the cheap-first order, under 1,500 characters), and
  `SKILL` is the Agent Skills file (`skill/spherecut/SKILL.md`, three
  recipes) that `spherecut agents install-skill` (the CLI's `skill`)
  writes to `$XDG_DATA_HOME/spherecut/skills/spherecut` and links into
  `~/.claude/skills` and `~/.agents/skills`. `docs/agents.md` is the
  user's guide, and tests keep its limits and codes in step with the
  code; `tests/recipes.rs` replays the recipes within their budgets.

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
* `TimeSpec` (`core::timespec`) is what a person types for a time:
  seconds with exact decimals, `m:ss` and `h:mm:ss`, a fraction, or a
  frame number with `f`, parsed by `FromStr` without `f64` and never
  negative. A frame number is counted from the stream's first presented
  frame; on a constant-rate stream `frame` seeks to it at `(N + ½)`
  frame durations, and on a variable-rate one it counts frames from the
  start (`media::seek::frame_address`).

## Rendering

Built in M1 slice 3 for equirect sources (`spherecut-render`, DECISIONS
D31; the golden images that hold it, D32, are under Testing tiers);
stitching and stabilization join the same pass later.

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
  `frame`, tests and benchmarks, and `Gpu::from_parts` wraps a device.
  `GpuCaps` says what it can do (timestamps, anisotropy, its largest
  texture). A lost device, or an error no error scope caught, is
  returned by `Gpu::check` as `gpu_failed`, which every entry point calls
  first; nothing panics in wgpu's error handler. All device creation is
  in one function, which asks for DMA-BUF import where the adapter has
  it (`GpuCaps.dmabuf_import`, slice 5b); the GUI opens its device there
  too, with `Gpu::for_window` before the window, and hands it to eframe
  through egui-wgpu's `WgpuSetup::Existing` (D31). `Gpu::headless_shared`
  and `for_window_shared` open it through `render::vkshare` instead,
  with Vulkan Video's extensions and queues for FFmpeg (slice 5b's path
  B); `Gpu::shared_video` gives the handles and the copier, or why the
  device couldn't be shared.
* **Slots** (`GpuFrame`, for a `SlotShape`) hold a frame's planes as
  canonical textures: `R8Unorm`/`Rg8Unorm` for 8-bit planes, and
  `R16Float`/`Rg16Float` holding code / (2^b − 1) for deeper ones, each
  with an area-weighted mip chain capped near 64 pixels (an 8-bit
  level's means of codes worked out in integers and rounded in the
  shader to the nearest code, ties to even, which a driver that converts
  to nearest, as Vulkan asks, stores as that code, and a deep level's to
  the nearest half, which every driver stores as it is, D31), plus the
  frame's `Source` uniform: its colour matrix, chroma
  siting and where it sits on the sphere (`Placement`: sv3d crop, stereo
  eye). Of a stereo frame a slot holds the shown eye alone
  (`Placement::shown`), so the other eye never bleeds in.
* **`Uploader::upload`** copies a frame's planes into a slot
  (`write_texture`, or a staging belt), runs the ingest pass that turns
  deep integer planes into f16 and the mip passes, and submits once,
  never waiting. Every plane is checked before any is written, so a
  refused frame leaves the slot as it was. A frame already on the GPU
  can fill level 0 itself, then call `Uploader::record_mips` and
  `GpuFrame::set_source`.
* **Zero copy** (slice 5b, D25): `render::dmabuf`, render's one `unsafe`
  module (D28), imports a VA-API frame's DMA-BUF layers as textures
  (`R8Unorm`, `Rg8Unorm`, `R16Uint`, `Rg16Uint`, one a plane), once the
  driver has said, through ash, that it takes the format's modifier
  from a DMA-BUF at that size. `Uploader::upload_imported` copies the
  8-bit planes GPU to GPU into the slot's level 0 and runs deep ones
  through the ingest pass, then the mips, as `upload` does. Each frame
  is imported afresh, and the caller keeps the frame until the GPU work
  that read it is done. Path B: `render::vkshare` opens the device
  shared, planning a decode queue and two compute queues wgpu never
  uses, one FFmpeg's and one its `Copier`'s. The copier copies a Vulkan
  Video frame's planes, under FFmpeg's frame lock and its timeline
  semaphore, into a buffer of its own; `Copier::order_before` makes
  wgpu's next submission wait for the copy, and
  `Uploader::upload_from_buffer` fills the slot from the buffer as the
  copy path does from its staging. What a device needs to be shared (its
  extensions, Vulkan 1.3, a decode family and a compute family with two
  queues besides wgpu's) is planned by `core::vkplan`, which media's
  doctor uses too, so that doctor's `shared_decode` is the renderer's
  answer (slice 9).
* **`Projector::record`** draws a slot through a `ViewParams`
  (orientation and `Lens`) into a `LinearTarget`: per pixel a ray from
  the lens, rotated into the world, its equirect uv, an LOD from central
  differences of the direction, a trilinear (or anisotropic) tap of each
  plane, and the 3×4 colour matrix and the sRGB EOTF. Pipelines exist per
  `Filter` and `DebugMode` and are built on first use.
  `Projector::record_flat_unchecked` is the flat pass (`fs_flat`, slice
  8): the picture's own texels through the clamp sampler, for the raw
  view and `StillOptions.flat`, against `reference::render_flat`.
  `GpuTimers` time the projection with timestamps inside the encoder
  where the device has them, read back a frame or two late and never
  waited for.
* **`Encoder::draw`** writes a linear target's pixels one to one into a
  render pass's attachment with the sRGB OETF, rounded in the shader to
  the attachment's codes (at 10 bits, and into an f16 attachment, to
  halves), which a driver that converts to nearest, as Vulkan asks,
  stores as they are, packed to f16 first or not (D31),
  at an origin, for egui's frame (slice 7) or an offscreen texture.
* **`render_still`** (and `StillRenderer`, which keeps its pipelines)
  does all of it headless and reads back an `Image`: 8-bit through the
  encode pass, 16-bit or linear from the linear target.
* **For export** *(M2.3, D49)*: a `ViewParams` also says what it
  renders (`out`: the lens, or the whole sphere as equirect, turned by
  its orientation) and may be framed (`Framing`: the lens spans a
  rectangle of the target and goes on past it, dimmed, for the
  reframer). `YuvConverter` (`yuv.wgsl`, compute) turns a linear target
  into an encoder's planes in a storage buffer (NV12, P010, I420 or
  I010; BT.709 limited range, chroma sited left, rows padded to 256
  bytes). `Blur` adds each of a frame's views into a `vec4<f32>`
  storage buffer and writes the mean back into the target.
  `output::frame_views` gives an output's size and a frame's views from
  `ViewAnim::at` (the shutter's, with blur), and `frame_views_at` the
  same at another size (a draft's); `StillRenderer::
  render_blurred` and `render_yuv` render them, and so does the export
  (M2.4), so the parity reference and the export render one frame.
* **`reference`** is the same projection in f64 on the CPU, with the
  GPU's storage formats emulated (equirect output, framing and the
  blurred mean too), and `yuv::reference` the YUV pass's; the Tier 1
  tests compare the two on lavapipe.

What is still planned *(M3 on)*, for every source type: in WGSL, "direction on the
unit sphere → texel(s)" for the GoPro EAC pair and calibrated dual
fisheye, with the view orientation composed with the stabilization
rotation, lenses blended across the seam, and the colour pipeline in
float before the output. Stitching, stabilization and reframing then
happen in one pass shared by the viewer and the exporter, and an
intermediate equirect exists only as the output of a 360 export.

## Export *(M2.4 and M2.5, D52)*

`spherecut_export::Export::start(ExportJob)` checks what needs no work
and runs the job on its own thread, which spawns two more:

```
decode (VA-API or software) ──2──▶ render (StillRenderer, YUV pass) ──2──▶ encode + mux (job thread)
  exact seek to the trim's start,     the clip's views at n/rate,       the encoder's planes (uploaded
  each source frame held for the      equirect or the lens, into        to a VA surface for VA-API), the
  output frames it covers             NV12 or P010 for VA-API,          trim's audio between frames,
                                      I420 or I010 for software         into a locked TempFile
```

The encoders are tried in order (M2.5): with `auto`, the codec's VA-API
encoder on the render node of the GPU that renders, then its software
one. A VA-API encoder that fails to open or on a frame in its first
second starts the video again in software from frame 0, and the
`Report`'s `fallback` says why; later failures fail the export. The
decode is VA-API on the same node going on in software (D3), and the
report says where frames were decoded. Each stage adds its busy time to
an atomic counter as it goes (`StageTimes`: decode, render, readback,
encode with the video's flush, and the last pass), so a slow export
names its slow stage.
`spherecut render` drives this handle from the command line (D51), and
`spherecut frame --project` renders a sequence's frame through the same
`frame_views` the render stage uses.

The channels hold two frames each, and one `StopSource` stops all
three. Once the muxer has finished, one last pass of media's injector
(`inject::rewrite_into` with `Finish`) writes the finished file beside
it with V2 and `SA3D` for 360, the V1 `uuid`, no video edit list and
the moov first; `check::check` reads it back through the probe (codec,
size, frames, duration, rate, layout, the spherical boxes, the audio),
and only then is it persisted at the output. A failure at any step, or
a cancel, removes both temporary files. Before it starts, the export
sweeps its folder of a killed export's temporary files (their locks are
free, D16) and refuses a disk with less free space than two copies of
the size estimate. `Progress` counts frames muxed and then the last
pass's bytes; the `Report` holds what was made and each check passed.

The app's export (M2.7, D55) renders on the viewer's device and queue.
Its `ExportJob` carries a `Pace`, shared with the UI: each UI pass says
whether the viewer plays and whether a drag runs. While the viewer
plays, each of the export's stages (decode, render, encode) starts no
frame until it stops (`Pace::stand_aside`, M2.10e), Cancel and a UI
that stops passing for 100 ms aside. While a drag runs, the render
stage waits before each frame for the UI's next pass,
or 100 ms at most, and its renderer keeps its jobs short
(`StillRenderer::share`, M2.10, D56): one submission in flight, a view
larger than 4K in bands of 4K's pixels, the readback in copies of 16
MiB. It renders on a GPU device of its own on the window's adapter
(`Gpu::beside`), so it shares no wgpu lock, queue or submit gate with
the window, only the GPU; the window's device is the fallback
(`SPHERECUT_EXPORT_DEVICE=window` asks for it). Each UI pass also says
whether the viewer has two frames ready beyond the one it shows, and
while a drag runs and it hasn't, the export's decode, render and encode
stages each wait before a frame's work (`Pace::yield_to_viewer`, 100 ms
at most), so the viewer's decodes go first on the GPU's video engine.
No thread but the UI's
waits for the GPU through wgpu's `Device::poll` wait, which holds the
lock a present takes: they use `Gpu::wait_for_submitted`, which polls
without waiting.

## The GUI *(M1 slices 7 and 8)*

`spherecut` with no command (`spherecut-app`, D33, D36):

* **Startup.** `run` finds a display by winit's rule (else exit 3,
  `no_display`), opens render's device with `Gpu::for_window` and an
  FFmpeg backend on that GPU's render node, and hands the device to
  eframe (`WgpuSetup::Existing`, one sample, no depth). eframe's creation
  callback marks the UI thread (`core::blocking`), puts
  `ViewerResources` into egui-wgpu's callback resources (the one write of
  that lock, before any frame), reads the saved tiles and settings,
  starts the `Player` with the device's audio, and opens the first file
  given.
* **One frame** is eframe's `logic` then `ui`. `SpherecutApp::logic`
  polls the player's events and snapshot, the doctor's job and the
  panes' jobs (never waiting) and picks the frames to show
  (`Player::present`); eframe runs it alone while the window is
  minimized or occluded, so playback goes on hidden, and `ui` runs it
  when nothing did (kittest). `ui` draws the menu, the notices, and the
  tiles: the viewer (the picture
  over the transport bar, or the strip with a project) beside the side
  panel's two tab rows. It
  asks for a repaint while anything moves (playback, a seek, a switch, a
  scrub) and every 50 ms while a job runs; the player's repaint
  callback wakes it when a frame comes up.
* **The viewer.** The picture's rect is rounded inward to whole physical
  pixels and inset by one (`viewer::pixel_rect`). `Player::present`
  gives the slot to show, and the paint callback's `prepare` projects it
  into the viewer's own linear target, of the rect's pixel size, only
  when its `DrawKey` (the slot, the frame in it, the view, the filter)
  differs from the last one drawn, or the size changed; `paint` encodes
  the target one pixel per pixel into egui's pass. A projection counts
  as drawn once `paint` ran after it: egui-wgpu drops what `prepare`
  recorded when the window's surface can't be acquired. `prepare`
  records without `Gpu::check`, so the UI thread never polls the
  device. A 30 fps clip on a
  144 Hz display projects 30 times a second.
* **Input.** A drag grabs the world direction under the pointer and
  keeps it there, level (Ctrl: free); scrolling and `zoom_delta` zoom
  about the pointer; double-click plays or pauses; the keys of the Help
  menu step, jump, seek to the ends, reset the view, zoom and switch the
  projection. The pointer is mapped through the rect `prepare` draws
  into, so the point under it is the pixel drawn there at any scale.
  Scrubbing seeks to keyframes while the pointer moves, the latest
  superseding the one before, and exactly once it rests 150 ms or lets
  go; the bar shades the cache's spans (`FrameCache::try_ranges`).
* **Notices.** The doctor runs on a worker at startup on the rendering
  GPU's node; its errors and warnings become banners with the fix and a
  Copy button, and its whole report is the Doctor tab's. The app adds its
  own issues (a CPU adapter, no audio device, no anisotropic filtering)
  and turns the player's events into notices (a fallback to software, a
  file without audio, a file that can't be opened or decoded, the GPU
  short of memory or lost, a cache too small). A notice of a kind shows
  once and stays dismissed; a file's notices go with it, and the
  machine's (the doctor's, the adapter's, a failed GPU, no audio device,
  a dropped slot) stay.
* **Files.** The Files pane lists a folder on a worker (folders first,
  then the video extensions and project files, hidden files left out)
  in place of a file dialog, and remembers the last folder.
* **The reframer** (M2.6, D54). `AppState::reframe` holds the open
  project (`doc::Doc`: the `Project`, its `History`, its file, the
  selected keys) and the jobs that make, load and save one, polled each
  frame by `reframe_tick`, which also gives the player the trim as its
  `PlaySpan`. Opening a video makes an untitled project from the view
  on screen. Each frame the picture first notes any change of the view
  (`watch_view`), then sets the keys' view at the presented frame's
  time unless a change is under way (`apply_keys`); a change becomes
  key edits when it ends, or a look-around. The viewer draws a flat
  output's frame (`Framing`) and maps the pointer through it. The strip
  (`timeline::strip_ui`) draws under the picture, collects what its
  input asks for and applies it once drawn, every edit one undo step
  through `AppState::edit`.
* **The reframer, part 2** (M2.7, D55). A drag in the curve editor,
  a number being changed in the Inspector, or a 360 output's front
  being turned changes a preview the `Doc` holds (`Doc::anim` and
  `Doc::output` give the view as shown), and one `AppState::edit` on
  release; any edit, undo or redo drops it. The curve editor (`curves`)
  samples the tracks once per two points and rebuilds a dragged key's
  orientation with one angle changed (`doc::with_angle`), keeping the
  neighbours' spins (`follow_spin`). The sphere overview (`overview`)
  draws its thumbnail through a paint callback of its own
  (`OverviewCallback`) with the viewer's projector, flat, into a
  texture redrawn only when the frame or its size changes. The export
  window (`export::window`) edits the sequence's output and export
  settings, checks audio copy on a job (`export-audio`), starts the
  export on a job (`export-start`) from a clone of the project, then
  polls the `Export` each frame (`export_tick`) and asks for a repaint
  every 100 ms while it runs; at exit a running export is cancelled and
  dropped on its own thread (`export-stop`), which `run` joins with the
  jobs. A drag's end checks for Escape (`app::escaped`), which lets it
  go unkept; after the panes have drawn, `let_go_of_lost_drags` ends any
  drag egui no longer has (its widget gone mid-drag). A viewer drag on
  a key keeps its turn's way with `Doc::follow_look`, the viewer's
  sticky spin, as the curve editor does.
* **Shutdown.** `on_exit` tells the player's threads to stop and leaves
  the player and the jobs' threads (`jobs::Threads`, which also holds
  those of jobs given up for newer ones) for `run`, which joins them
  after the event loop, off the UI thread: the player with 2 s, the jobs
  with 3 s.
* **Never blocking.** `crates/app/clippy.toml` bans the blocking calls
  (locks, waits, `recv`, `join`, `sleep`, file reads, `Device::poll`,
  `block_on`, `Player::join`); workers and the code around the event loop
  that make them say why. The runtime guard covers the other crates'
  code the UI calls, and the GUI's `never_blocks` test runs the whole
  app on a backend that never answers.
* **The overlay** (F3 or View ▸ Overlay, slice 8, D40) is labels over
  the viewer's top-left corner, read once a UI frame from `PerfStats`,
  the snapshot and the audio output's stats, all without a lock that
  waits: where the first stream decodes and on which GPU; decode and
  download; upload's CPU and GPU time; the projection's CPU time and
  its GPU time from timestamps (`GpuTimers`, read back a frame late,
  "n/a" without `TIMESTAMP_QUERY_INSIDE_ENCODERS`, "(CPU timing)" on
  lavapipe); the viewer's size, scale and filter, and the share of the
  last second's draws that only encoded; UI frame times and hitches;
  frames shown, dropped and decode-dropped; the A/V offset's median and
  p95 over the last second; audio; the cache and the slots; RSS, minor
  faults a second and the GPU allocator's bytes, which the upload
  thread reads once a second. Sparklines show the UI's and the stages'
  last samples.
* **Settings** (File ▸ Settings…, slice 8, D40) are kept in eframe's
  storage beside the tiles: the cache's MiB or automatic, the decoder
  (automatic, software, VA-API or Vulkan Video only), the GPU slots,
  the A/V offset, anisotropic filtering and the overlay. A change
  reaches the player at once (`Player::set_config`). `--cache-mb` and
  `SPHERECUT_FRAME_CACHE_MIB` win over the cache setting, which the
  window then shows as theirs and greys out. A stored copy out of range,
  or from an older version, loads with its values brought into range
  and the missing ones at their defaults.
* **The raw view** (slice 8, D40) shows a source that isn't equirect,
  or a camera's raw file with one video stream a lens, flat: a column
  per stream, each picture as large as its aspect fits and centred,
  black around it (render's flat pass, `Projector::record_flat_unchecked`,
  into a `LinearTarget` per column, painted under a scissor each). A
  drag does nothing there. The file opens on its best stream; when the
  Info pane's probe finds two or more video streams of one size (cover
  art aside), the app opens it again with all of them, and a notice says
  stitching arrives in M3.
* **The smoke run** (`spherecut --smoke <clip> --smoke-out <json>`,
  hidden, slice 8, D37 and D40) drives the app from its own passes
  (`smoke::SmokeDriver`): it opens the clip with the default settings
  and the overlay on, waits for its first frame, plays it for
  `--smoke-seconds` or to its end, gathers every stage's samples as they
  come (`PerfStats::samples_after`), takes a screenshot
  (`ctx.request_screenshot`, downscaled to 960 pixels wide), writes
  `spherecut.smoke/1` (`docs/cli-json.md`) and closes the window. The
  files are written on a worker. The stored settings and layout are
  neither read nor written. A watchdog thread ends a run whose window
  has had no UI pass for 15 s (a locked screen, slice 9), writing a
  `timed_out` document that says whether eframe's logic still ran, and
  exits 1. With `--smoke-export <MP4>` (M2.7) it exports the clip's
  first 2 s once settled, plays on while it runs, and waits for it to
  end; the document's `export` holds its outcome and the run's numbers
  over the time it ran. hw-test's Tier 2 `gui_smoke` runs it on each clip in the
  desktop session it finds, unless logind says it is locked, and
  kittest runs it with no compositor.
* **The timeline prototype** (`spherecut --timeline-proto`, hidden,
  M2.2b, D53) is the toolkit check's (D26): `proto::TimelineProto`, on
  the device the viewer would take, draws 500 seeded clips on 6 tracks
  with their own thumbnail textures and lets them be moved with
  snapping, ripple trimmed, scrubbed, zoomed and panned. With
  `--proto-out` a script (`TimelineProto::script`) adds its input to
  each pass's raw input (eframe's `raw_input_hook`), and the run writes
  `spherecut.timeline-proto/1` and a screenshot as the smoke run does.
  It shares only eframe's setup with the app (`run::native_options`).
  hw-test's Tier 2 `gui_proto` judges it, and kittest runs it.

## Threads *(as built in M1)*

* UI thread: egui, never blocks on decode, disk or GPU readback; the
  runtime guard and `crates/app/clippy.toml`'s bans hold it to that
  (slices 5 and 7, below and under the GUI).
* Decode workers per open source, feeding a bounded frame cache (slice
  5, below).
* Audio (slice 6, below): the output's clock is the playback clock, and
  video presentation follows it.
* The GUI's jobs (`app::jobs::Job`, slices 7 and 8): a named thread per
  piece of work the UI would otherwise wait for, whose result the UI
  polls each frame: `doctor` (the startup check), `probe` (the Info
  pane), `files` (a folder's listing), `smoke-out` (the smoke run's
  PNG and JSON), the reframer's loads, saves and file checks (M2.6),
  and the export's `export-audio`, `export-start` and `export-stop`
  (M2.7). A job whose thread panics or can't start is lost,
  which the UI reports, and at exit
  `run` joins them beside the player's threads, with 3 s. The smoke
  run's `smoke-watchdog` is the one thread never joined: it ends with
  the process, or ends the process itself (above).
* *(planned, M2 on)* Background workers for thumbnails, waveforms,
  proxies and export, each with progress reporting and cancellation
  tokens.
* Cancellation: `core::cancel`'s `StopSource` hands out
  `StopToken`s by generation; starting a new generation stops the older
  tokens, as a new seek stops the last one, and shutdown stops them all
  and raises the flag a decoder's FFmpeg interrupt reads (D13, D29).
* Built in slice 4: `spherecut frame` does its work on one worker
  thread while the main thread polls the SIGINT flag every 10 ms and
  shuts the `StopSource` down on the first Ctrl+C (`spherecut frame`,
  above).
* Built in slice 5 (D34), in `spherecut-playback`:
  * **Data flow.** A file's frames go one way: each video stream's
    `decode-v{i}` worker (on the decoder `playback::decoder` picks from
    doctor's checks) puts decoded frames into the `FrameCache`; the
    `upload` thread copies the ones the presenter will want into the
    `SlotRing`'s GPU slots; `Player::present`, on the UI thread, picks
    each stream's slot for the next refresh; and the viewport draws it
    (slice 7; the Tier 2 benchmarks' offscreen target until then).
    Commands go the other way, on unbounded channels, each request with
    a `StopToken` of a new generation; what the threads did comes back
    through the slots' atomics, a status read with `try_lock`, events
    and `PerfStats`.
  * **The UI thread never blocks.** The GUI marks its thread with
    `core::blocking::mark_ui_thread()`, and test harnesses mark theirs.
    Every helper that can block calls `may_block(what)` first: the
    frame cache's lock and waits, the player's status lock, a channel's
    `recv`, a thread's `join`, file I/O, opening, probing, seeking and
    decoding media, opening a GPU device, and the readback with its
    `Device::poll`. On a marked thread that panics in debug and test
    builds, and is logged once in release builds. The `Player`'s
    methods for the UI thread only send on unbounded channels, `try_lock`
    and use atomics.
  * **The threads.** Per player, one `upload` thread for its life. Per
    open, a transient `open` worker (it picks each stream's decoder,
    running doctor's decode checks once if it must, opens the file's
    audio (slice 6, below), clears what the file before left, and
    starts the decode workers), and a `decode-v{i}` worker per video
    stream. A request (a seek or a step) goes to every stream's worker
    with a new `StopToken` generation, which stops the one before at
    its next frame or wait.
  * **The frame cache** (`playback::FrameCache`, a `Mutex` and a
    `Condvar`) sits between the decode workers and the upload thread.
    It holds `Arc<VideoFrame>`s by stream and clip time, linked by run,
    within a budget sized from the memory available and any cgroup
    limit (`playback::budget`, a cgroup's use being its working set),
    rechecked every 5 s on the upload thread with the cache's own
    frames counted as free, leaving 1 GiB available where it can. A worker's
    frame ahead of the playhead past the window waits for room, in
    20 ms slices that check its `StopToken`. A frame at or behind the
    playhead never waits, and the frame a request landed on is pinned
    while that request is the target. A seek into frames a run holds is
    answered from it, and decoding goes on from its end. A step's worker
    moves the playhead to where it starts, then frame by frame. Frames
    still on a decoder's surface (zero copy, slice 5b) are held to
    `SURFACE_CAP` a stream, the window one fewer, on top of the budget,
    since the decoder's pool has only so many.
  * **Zero copy** (slice 5b, D25, `PlayerConfig.zero_copy`, set by
    `SPHERECUT_ZERO_COPY`: Vulkan Video's frames by default, VA-API's
    too with `1`, none with `0`): on a device that imports DMA-BUFs, and
    with VA-API's asked for, a VA-API stream's worker asks for DRM PRIME
    frames, and the upload
    thread imports their planes (`render::dmabuf`) in place of the copy,
    keeping each frame until `Queue::on_submitted_work_done` for the
    submission that read it. On a device opened shared (path B) a
    Vulkan Video stream decodes through FFmpeg's context on it, made by
    the first open that wants one, and the upload thread copies each
    frame out with the copier, waits, lets the frame go and holds the
    buffer until the GPU has read it. A refused import or copy is
    copied through the CPU, said once in the snapshot
    (`import_refused`), and the next opens ask for downloads.
  * **The submit gate** (D41, `core::gate::SubmitGate`, one a device,
    `Gpu::submit_gate`): while the UI thread holds a swapchain image,
    no other thread submits to its device, since RADV makes every
    submission then a writer of that image and the compositor waits
    for it. The viewer's `prepare` closes it before egui-wgpu acquires
    the surface, its `paint` opens it 2 ms on, after egui's submit and
    the present, and the next pass's logic opens it at once; one left
    closed opens after 20 ms. FFmpeg's queue locks (`lock_queue`, ours
    on a shared device), the copier and the uploader wait at it right
    before they submit; the UI thread never does.
  * **GPU slots** (`playback::SlotRing`): 3 to 6 `GpuFrame`s, each with
    an atomic state word (Free, Writing, Ready, Shown, Dead, and a
    version). The upload thread claims a Free slot only when the ring's
    `Arc` is the only one, so no draw still holds it, uploads the frames
    the presenter will want next (from the frame at the playhead on
    while playing, as far as the frames follow on, so that frames a seek
    back left ahead past a gap wait; it, the next and the one before
    while paused), and
    frees the Ready ones no longer wanted, never the Shown one. While a
    stream's frame for a request, or with fewer than two slots a stream
    its frame at the playhead, isn't in a slot, every stream wants its
    own alone, so no stream's next frame takes the slot another needs.
    With no more live slots than streams nothing can land: slots die
    only on GPU out-of-memory, and the viewer is to treat that as a GPU
    failure. The presenter, on the UI thread, shows a Ready slot by
    compare and swap.
  * **The presenter** (`Player::present`) picks each stream's latest
    Ready frame at or before the clock's time at the next refresh, and
    after a seek keeps the frame on screen until the new one is Ready
    (with fewer than two slots a stream, each stream's goes up as it
    comes), and while playing starts the clock once the two frames after
    it are Ready too, or the stream ends. A step made while a request is on its way goes on from
    where that lands, a keyframe seek's included.
  * **Clocks** (`core::clock`): `SystemClock` and `ManualClock` read
    through a sequence lock of atomics, so the UI thread reads them
    every frame without a lock. A file's audio brings its own clock,
    which video then follows (slice 6, below).
  * **Shutdown**: `begin_shutdown` stops every token and raises the
    decoders' interrupt flag, and returns; `join(budget)` waits for the
    threads, polling `is_finished`, and says which are left.
  * **Numbers** (`playback::PerfStats`): each thread records its stages
    into rings of atomics, and the overlay reads them without a lock.
* Built in slice 6 (D35), in `spherecut-audio` and `playback::audio`:
  * **Data flow.** For a file with audio, the open worker opens an
    `Output` (the default device, else the null output) and starts
    `audio-decode`, which waits for stream 0's worker to give clip time
    0 and then decodes into the output's chunk ring: an SPSC ring of
    512-frame chunks, about 500 ms, of atomics, each chunk stamped with
    its epoch and first sample. The output's callback, on cpal's
    real-time thread (or `audio-null`'s, or a test's for the fake
    device), fills each buffer from the ring by sample index and
    anchors the clock to it. It takes no lock, allocates nothing and
    logs nothing.
  * **The clock** (`AudioClock`) is a `Clock` in a sequence lock. A seek
    or a pause starts an epoch, which the decoder seeks to and the
    callback flushes by. Playing, it waits at its base time until the
    epoch's audio plays, then runs from each callback's anchor, at most
    the latency and two buffers past it, never back. When the device is
    lost, stalls for 500 ms or brings no audio for 2 s it runs on the
    system clock, and a callback brings it back to the audio without a
    jump.
  * **`audio-out`** owns the cpal stream: it plays and pauses it with
    the clock, runs the watchdog every 50 ms, counts xruns, and on a
    lost device tries the default one again every 2 s at the same rate
    and channels.
  * **The UI thread** reads the clock through the sequence lock and the
    output's numbers with `try_lock` (`Player::audio_stats`). A preroll
    waits for 100 ms of audio queued, at most 2 s (`AudioLate`). Read a
    present's latency ahead, the clock's time is held at the last
    present's while playing, so a late callback never takes video back.
  * **Ending.** `audio-decode` and the output's thread are the player's
    (`PlayerThreads`): `audio-decode` ends with the open's token, at a
    newer open or the shutdown, which also interrupts its reads, and
    hands the output's thread over as it goes, so `join` waits for and
    reports both. Dropping an output never waits.

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
  mips, both upload paths, `render_still` against the CPU reference, and
  the golden images (below). The CLI's tests (`crates/cli/tests/`) run
  the binary: the JSON contract with every exit status (a Ctrl+C in the
  middle of a seek included), FFmpeg's confinement under strace, and
  `frame` on lavapipe (the Euler table, the frame code read back, every
  error row with nothing left behind, the output races, the pipeline
  goldens). The playback engine's tests (`crates/playback/tests/`) run
  on lavapipe, and on a `ManualClock` wherever wall time would make
  them flaky: the frame cache against a model of its rules, the
  engine's rules with synthetic and generated media (a 5.7K source in a
  256 MiB cache among them), a fake UI loop on a marked thread that
  must never wait, through a backend that never answers and a storm of
  seeks (`never_blocks.rs`), the Tier 2 playback benchmarks'
  harness, in software on a small clip (`tier2_playback.rs`), and A/V
  sync through the engine on the fake audio device, whose callbacks come
  at scripted instants, and on the null output (`av_sync.rs`). The audio
  decoder's tests (`crates/synth/tests/audio.rs`) check samples, seeks,
  resampling and channels on clips synth writes, and the audio crate's
  (`crates/audio/tests/clock.rs`) the clock on the fake and null
  devices.
  `cargo xtask check` runs all of it, against the FFmpeg pkg-config
  finds. `cargo xtask compat` runs clippy against other FFmpeg builds
  and builds and links the tests against each, in a target directory of
  its own that is deleted when the FFmpeg behind its prefix changes (and
  only while it holds compat's stamp), checks before and after that
  every library the build bound came from the prefix, and
  with `--test` runs Tier 1 against one; `check` runs the clippy and the
  link for the prefixes `SPHERECUT_FFMPEG_COMPAT` lists, and says when it
  lists none (D2).
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
  the details. Unless `--quick`, hw-test first makes the perf clips the
  playback benchmarks play (synth's noisy 5.7K and 8K HEVC at a camera's
  bitrate, kept in `<target>/hw-media/` across runs), and after Tier 2
  it runs Tier 1 on the machine (`--no-tier1` leaves it out). The report
  also records the power settings and every render node, the display the
  GUI opens on (`WAYLAND_DISPLAY`, or from a TTY the user's one Wayland
  socket, or `DISPLAY`), the audio server, GNOME's mutter features and
  Hyprland's version (D37). Its "GUI smoke run" is `gui_smoke`'s
  (`crates/cli/tests/tier2_gui.rs`): `spherecut --smoke` for 20 s on the
  perf clips, a seam clip, the sync clip, each stitched sample and an X5
  raw file, each run's numbers and verdict in a table and its screenshot
  beside the report (`<report>-gui-<n>.png`). A run fails when under 90%
  of the UI frames expected were presented (a hidden session), over
  0.1% of the frames dropped, UI p99 is over 1.5× the refresh period,
  |A/V| p95 reaches 20 ms on the sync clip, or HEVC decodes in software
  where doctor says VA-API decodes it; without a session it skips, and
  with the session locked (logind's `LockedHint`) it fails at once
  and the report says so. The
  playback benchmarks (`crates/playback/tests/tier2_playback.rs`) play
  the perf clips and the X5's samples through a `Player` on the GPU at
  60 Hz deadlines into an offscreen 1920×1080 target, unthrottled and in
  real time, and gate 5.7K real time on the hardware decoder `Auto`
  picks (D34). `cargo xtask hw-test --list` lists the Tier 2 tests.

### Golden images

`crates/render/tests/goldens.rs` holds the renderer to pictures known to
be right (DECISIONS D32). One test, `goldens`, runs each row of the M1
plan's table (section 6) on lavapipe, from synth's in-memory planes:

* six views of synth's grid are **stored** as PNGs in
  `crates/render/tests/goldens/`, and must come out pixel for pixel the
  same at the Mesa build they were blessed on, and on another within
  PSNR 48 dB and SSIM 0.995, with at most 4 pixels more than 4 LSB off
  and none more than 8, which catches the local faults the averages
  miss (implicit LOD round a pole or along a minified line, a black pole
  quad);
* views are held to **the f64 CPU reference** at a PSNR floor set when
  blessing, and minified ones to **the supersampled pattern itself**
  (`synth::render_view_ideal`), with a margin over a level-0 render;
* **the seam and the poles** are placed inside a 2×2 pixel quad and
  compared with a roll twin (the texture rolled by half its width, the
  view turned 180°), pixel for pixel;
* the other rows check one property each: shimmer as the view turns, a
  10-bit ramp, plane layouts and strides, flat fields through the f16
  target, ranges, matrices, chroma siting, an sv3d crop and the LOD heat
  map.

Every row has a negative control, asserted on every run (implicit
derivatives must show the seam, level 0 must shimmer, a swapped range,
matrix or siting must miss), so a check that stops seeing what it checks
fails. The colour rows also have truths that don't go through render's
colour code, which the CPU reference shares: the BT.601 and BT.709
codings of one picture must render alike, and smooth chroma at 4:2:0
must match the same chroma at 4:4:4. `goldens/manifest.json` records,
for every row, the driver build and wgpu version, the commit and date,
every source it renders and the hash of its planes, every view, the
thresholds and every measured number. A source whose planes hash
otherwise than blessed is reported as "the synthetic source changed",
never compared as a render.

To bless, after a change meant to move the pictures, or a Mesa update:

```sh
SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test goldens
```

It writes the PNGs and the manifest only when every row passes and
every control fails as it should; otherwise it lists what failed and
writes nothing. It prints every calibrated floor as old → new, and
refuses one that would drop by more than 0.5 dB: a render that got
worse, or another driver build. When the drop is meant, as it may be
after a Mesa update, bless with `SPHERECUT_UPDATE_GOLDENS=lower`. Look
at the changed images and the manifest's numbers before committing
them. A failing run writes `<row>-<what>-{got,want,diff}.png` to
`golden-diffs/` in the cargo target directory (`target/golden-diffs/`,
or under `CARGO_TARGET_DIR`).

On a real machine, the Tier 2 `goldens_on_gpu_match_reference` runs the
rows on each GPU against the references and controls only, never
against the lavapipe PNGs, and `goldens_on_host_llvmpipe` runs the
stored set on the machine's own llvmpipe by the other-driver rule; both
note their numbers for the hw-test report, a GPU's CPU checks held to
lavapipe's blessed floors with their error broken down by LOD band.
When the mip pass left its 8-bit means to the driver, RADV's export,
which packs each to f16 toward zero first, cost the RX 7600's wide,
minified views up to 15 dB against the CPU reference; emulated on the
CPU, it reproduces those numbers. With the means rounded in the
shader the RX 7600 comes out at the emulation's own numbers, 58.88 to
60.75 dB, 8.57 dB or more above lavapipe's floors (odin at 918d031,
D32).

A second, smaller set, the **pipeline goldens**
(`pipeline_goldens` in `crates/cli/tests/frame.rs`, PNGs and manifest
in `crates/cli/tests/goldens/`), goes from a real lossless file through
the whole of `spherecut frame`, decoding included, under the same rules,
blessing gate and manifest fields, with `frame --reference` as the CPU
check and FNV-64 of the first decoded frame's planes as the source hash:
10-bit HEVC as `yuv420p10le`, full-range H.264, NV12 against `yuv420p`,
4:2:2 10-bit at full chroma resolution, and the swscale cross-check of
4:4:4 at BT.601 and BT.709, both ranges, 8 and 10 bits (D32). Bless it
with `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test
frame pipeline_goldens`.
