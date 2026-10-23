# Decisions

Each entry says what was decided, why, what else was considered, and what
would make us revisit it. Heavy dependencies are justified here, as the
project rules require. Newest entries go at the end; superseded entries stay
and say so.

## D1. Rust stable, edition 2024, MSRV 1.88 now and 1.95 with the GUI

*2026-10-22, M0.*

The workspace builds on stable Rust with edition 2024. `rust-version` is
1.88 today, which is what the current crates need (let-chains). The egui
0.37 family needs 1.95, so the workspace moves to 1.95 when the render and
app crates land in M1. Fedora 44 ships Rust 1.95 as an RPM, so this does
not force rustup on Fedora, but the RPM has no rustfmt or clippy, so
`cargo xtask check` on Fedora needs `rustup` or the `rustfmt` and `clippy`
RPMs.

Checked 2026-10-23 at 3d06a2e: `cargo +1.88 build --workspace
--all-targets` builds without warnings and `cargo +1.88 test --workspace`
passes.

## D2. FFmpeg through ffmpeg-next, hardware paths over ffmpeg-sys-next

*2026-10-22, M0. Research: `docs/research/ffmpeg-bindings.md`.*

`spherecut-media` uses `ffmpeg-next =9.0.0` for demux, decode, encode, mux,
swscale and swresample, and calls `ffmpeg-sys-next =9.0.1` directly for
hardware devices, `get_format` and frame transfer. All raw FFI lives in
`crates/media/src/ffi.rs` behind RAII types (`HwDevice`), each `unsafe`
block with a SAFETY comment. FFmpeg is the system library, linked
dynamically through pkg-config.

Why:

- It builds against every FFmpeg our target distros ship: Ubuntu 24.04 has
  6.1, Fedora 43 has 7.1, Fedora 44 has 8.1, and Arch and Fedora 45 have
  9.0. Checked on 2026-10-23 at 59976e9: against Ubuntu's 6.1.1 every
  Tier 1 test passes. Against a 9.0.2 built here with VA-API, libx264,
  libx265 and libdav1d but no Vulkan, every media, CLI and synth test
  passes except the two in `crates/cli/tests/doctor.rs`, which need
  FFmpeg's Vulkan device. Minimal 7.1 and 9.0.2 builds compile, and 122
  of the 149 Tier 1 tests pass on each. The same 27 fail on both: 21 need
  libx264 or libx265, 2 need audio encoders those builds leave out (Opus,
  Vorbis, FLAC), 3 are doctor tests that need the AV1 and ProRes decoders
  and FFmpeg's Vulkan device, and 1 needs the `lrc` demuxer.
  An 8.1 build was checked in an earlier round, before the M0 fixes.
  *Updated after the final review:* the three tests in
  `crates/media/tests/hw_refusal.rs` decode on FFmpeg's Vulkan device
  too, so they also fail on the 9.0.2 build without Vulkan.
- Since 9.0 it binds `hwcontext_vaapi.h`, `hwcontext_vulkan.h` and
  `hwcontext_drm.h`, which later zero-copy work (VA-API to DMA-BUF to
  Vulkan) needs.
- It is maintained (Gyroflow's maintainer is the main author), and Gyroflow
  is a GPL-3.0-or-later reference for the same hardware paths.

Considered: rsmpeg (doesn't build on FFmpeg 9.0, and pulls in `paste`,
which has an unmaintained advisory), ffmpeg-the-third (doesn't bind the
VA-API or Vulkan hwcontext structs), our own bindgen layer (a large build
script to maintain for version detection, with the same result).

Versions are pinned with `=` because ffmpeg-next's minor versions may
break its API. Move to 9.0.1 when it is on crates.io.

**License.** ffmpeg-next and ffmpeg-sys-next are WTFPL. The FSF lists the
WTFPL as GPL-compatible. `deny.toml` allows it for those two crates only,
as a per-crate exception, rather than in the global allow list.

Revisit if: ffmpeg-next stops tracking new FFmpeg releases.

*Updated by D11:* `crates/media/src/hwprobe.rs` (doctor's probes) is a
second FFI module, and C enums are read as ints, never as bindgen enums.

## D3. Hardware decoding never falls back to software silently

*2026-10-23, M0.*

When a caller asks `VideoDecoder` for a hardware device, the `get_format`
callback refuses every format except the device's, and every frame is
checked to have come from the device. If the device can't decode the
stream, the caller gets an error with the reason. Callers choose the
fallback: the app will fall back to software with a visible notice, and
Tier 2 tests fail.

Why: the hardware survey on huginn showed `ffmpeg -hwaccel vaapi`
"succeeding" by quietly decoding on the CPU, because Fedora's Mesa VA-API
driver ships without H.264 and HEVC. A test that only checks for decoded
frames would have passed. The project rules say Tier 2 must never pass
silently.

Software decoding lets libavcodec choose the thread count (frame
threading). libavcodec's default is one thread, which is several times
slower at 5.7K. A hardware decoder keeps that default.

*Extended by D14:* the error now says why the device refused the stream
(`MediaError::HwUnsupported`), and doctor's probes use the same check.

## D4. Our own MP4 box parser and injector for spherical metadata

*2026-10-22, M0.*

`spherecut-media::mp4` parses ISO-BMFF boxes, and `spherical` reads
Spherical Video V1 (the XML `uuid` box), V2 (`st3d`, `sv3d` and its
children) and `SA3D` from the file's own boxes. `inject` writes V2 and
SA3D into the first video and audio sample entries after FFmpeg has
muxed, and fixes ancestor box sizes and `stco`/`co64` chunk offsets.

Why: FFmpeg's demuxer exposes spherical metadata differently across
6.1 to 9.x, doesn't expose V1 XML, and doesn't tell us which boxes were
present. FFmpeg's MP4 muxer writes `st3d`/`sv3d` only with
`-strict unofficial` and doesn't write `SA3D`. Export has to write all of
them and the prompt requires re-parsing to validate. One parser for
reading, injecting and validating keeps those consistent. Camera-specific
boxes (Insta360 trailer, GoPro `udta`, DJI metadata) also need it.

Validation: FFmpeg 6.1's `ffprobe` reads our injected boxes back as side
data `spherical: equirectangular` and `stereo3d: 2D`.

*Updated by D15 and D16:* this entry used to say the parser was fuzzed.
At the time it was tested with 2000 byte flips of one file plus
truncations. D15 has the parser's limits, its damage checks and the
fuzzing now in place; D16 has inject's checks and atomic writes.

## D5. glam for vectors and quaternions

*2026-10-22, M0. Research: `docs/research/gpu-ui-stack.md` §8.*

glam 0.34. `DQuat` (f64) for orientation in the model and stabilization
math, `Quat`/`Vec3` (f32) at the GPU boundary. Rotation convention:
`world = q * view` (ARCHITECTURE.md).

Considered: nalgebra, which is heavier and generic. Add it later only in
the crate that needs real linear algebra (lens calibration, M3), if any.

## D6. Our own exact rational time type

*2026-10-22, M0.*

`spherecut_core::time::{Rational, Time, FrameRate}`: i64/i64 reduced, with
i128 intermediates, checked arithmetic and explicit rounding when
converting to a stream's timebase. Considered: num-rational, whose
`Ratio<i64>` overflows on intermediate products unless every call site uses
the `Checked*` traits, and which hasn't been released since 2024.

## D7. GUI and GPU stack (for M1)

*2026-10-22, decided ahead of M1. Research: `docs/research/gpu-ui-stack.md`.*

- wgpu 30.0.1 with only the Vulkan backend in shipped builds. lavapipe for
  headless tests.
- eframe, egui, egui-wgpu and egui_kittest 0.37.0 (these need wgpu 30 and
  winit 0.30.13).
- Docking with **egui_tiles 0.18.0**, not egui_dock: egui_dock has no
  release for egui 0.37 on crates.io, and `deny.toml` rejects git
  dependencies. egui_tiles has no floating windows. Revisit when egui_dock
  0.22 is published, if floating panels are wanted.
- Decoded frames are uploaded as one texture per plane (R8 + RG8, or R16 +
  RG16 for 10-bit) and converted to RGB in WGSL. This runs on lavapipe,
  RADV and ANV, and matches the only DMA-BUF import wgpu 30 has
  (single-plane).
- wgpu and egui-wgpu move together: adopt wgpu 31 only with the egui release
  that moves to it.

## D8. Supply chain and no runtime network

*2026-10-22, M0.*

`cargo deny` and `cargo audit` run in `cargo xtask check`. `deny.toml`:

- licenses: a GPL-compatible allow list, plus the WTFPL exception in D2;
- bans: HTTP clients (`reqwest`, `hyper`, `ureq`, `isahc`, `curl`) and
  webview crates (`wry`, `tauri`, `webkit2gtk`), so a dependency can't
  quietly add network access or a webview (*updated by D21*, which adds
  more HTTP clients and the TLS stacks);
- sources: crates.io only; unknown registries and git sources are denied.

The app makes no network requests. `spherecut doctor` prints the exact
`dnf`/`pacman` command for a problem; it never runs package managers or
downloads anything.

*Superseded in part by D18:* doctor still never installs or changes
anything and downloads nothing, but on Fedora it makes one read-only
`rpm -q` query to word its fixes. `cargo xtask hw-test` also runs `rpm -q`
in its prerequisite check.

## D9. Test tiers and synthetic media

*2026-10-22, M0.*

- Tier 1 runs anywhere, including this GPU-less cloud VM, with lavapipe for
  GPU work once it exists. *Updated:* lavapipe is needed already, because
  `crates/cli/tests/doctor.rs` runs doctor with the Vulkan loader limited
  to it (D18), and `crates/media/tests/hw_refusal.rs` checks that
  `VideoDecoder` turns streams down on it (D14).
- Tier 2 tests are `#[ignore = "tier2: ..."]` and call
  `spherecut_testkit::tier2::require_*`, which panic with a `TIER2-SKIP:`
  marker when hardware or samples are missing. `cargo xtask hw-test` runs
  them with `--ignored` and counts that marker as "skipped" with its reason.
  So a missing GPU is never a pass and never a mystery failure.
  *Updated by D19:* when this was written, hw-test could drop tests whose
  binary never ran (review B1), so the last sentence did not hold. D19 has
  the reporting rules that make it hold now.
- Test media is generated, not committed. `cargo xtask gen-media` renders
  patterns with known geometry (axis markers with letters, a 16-bit frame
  code, a moving marker) and encodes them with libx264/libx265
  single-threaded, so the pattern frames are reproducible. Lossless
  variants allow bit-exact round-trip checks. Tier 1 therefore needs an
  FFmpeg with libx264 and libx265 (on Fedora, RPM Fusion's `ffmpeg`, not
  `ffmpeg-free`). With doctor's checks (D18) it also needs libdav1d and an
  FFmpeg built with Vulkan.
- Determinism: a given build renders the same bytes on every run.
  Agreement across machines is expected but not guaranteed, because the
  pattern code uses libm trigonometry, and it has not been checked. Two
  full `gen-media` runs on the fix/writers branch gave byte-identical
  output (sha256 over all 19 files).
- `spherecut_synth::media::write` writes through `Mp4Writer` (D16), so a
  clip at its final name is always complete. The Tier 2 clip cache
  (`CARGO_TARGET_TMPDIR/tier2-media-v2`) relies on that. A killed run can
  leave hidden `.spherecut-*.tmp` files.
- Real camera samples stay outside the repo (`samples/` is gitignored) and
  are only used by Tier 2 tests.

## D10. GPL-3.0-or-later, and reuse of GPL code

*2026-10-22, M0.*

The project is GPL-3.0-or-later. GPL-3.0-compatible code may be reused with
attribution in the file header, notably Gyroflow (GPL-3.0-or-later) for
FFmpeg hardware paths and lens/IMU math. Closed vendor SDKs (for example
the Insta360 Media SDK) are not used, at build time or run time.

## D11. FFmpeg's C enums are read as ints, never as bindgen enums

*2026-10-23, M0. Updates D2.*

FFmpeg's C enums (pixel and sample formats, colour fields, media types,
codec ids, hardware device types) never become bindgen's Rust enums. A
Rust enum may hold only the values in the headers ffmpeg-sys-next was
generated from, and a newer libavutil at run time can hand back others,
such as a new pixel format. Making a Rust enum from such a value is
undefined behaviour. So `ffi.rs` declares the FFmpeg functions that take
or return these enums with the `c_int` the C ABI passes, and reads enum
fields as ints.

The rule covers ffmpeg-next's own accessors that build these enums from C
fields: `Context::id()` (which `Decoder::video` calls), `medium()` on
`Opened`, `Context` and `Parameters`, and `Frame::format()`, which
`frame::Video::data()` also calls. The decode and probe paths use
`ffi::open_video_decoder`, `ffi::media_type`, `ffi::plane` and
`ffi::frame_format` instead.

The same goes for strings. ffmpeg-next's dictionary iterator makes a
`&str` of each key and value without checking that it is UTF-8, and
FFmpeg copies tags as the file has them. So `probe` reads metadata with
`ffi::dict_entries`, which turns bytes that aren't UTF-8 into U+FFFD
(review C2). `ff::format::input` and `input_with_interrupt` unwrap the
path as UTF-8 and panic on any other Linux file name. So `probe` and
`VideoDecoder` open files with `ffi::open_input`, which gives FFmpeg the
path's bytes as they are (review C1). Refusing such names with
`MediaError::Open` was considered, but FFmpeg reads those files fine.

FFI lives in two modules: `ffi.rs` and, for doctor's probes,
`hwprobe.rs`. The workspace denies `unsafe_op_in_unsafe_fn` and
`clippy::undocumented_unsafe_blocks`, so each `unsafe` block has a SAFETY
comment.

Why: the M0 review (S3) found `transmute` from a `c_int` into
`AVPixelFormat` and `AVSampleFormat`.

Considered: checking each value against the enum's `*_NB` count before
converting. The count also comes from the build headers, so a valid value
from a newer FFmpeg would still be refused.

Not covered yet: the encode paths (`Mp4Writer`, and doctor's encode and
decode probes) still call `Encoder::video`, `Encoder::audio`,
`Opened::video` and `Frame::data_mut`. There the codec is chosen by name
and the frame formats are set by us, so the enums only hold values we
chose. `Mp4Writer`'s check of the audio encoder's sample formats reads
the runtime FFmpeg's list as ints (`ffi::sample_formats`): through
`avcodec_get_supported_config` on FFmpeg 7.1 and later, and
`AVCodec.sample_fmts` before that. `crates/media/build.rs` tells the two
apart from ffmpeg-sys-next's version flags. Some tests
(`generated_media.rs`, `writers.rs`, the `hwprobe` unit tests, the `ffi`
plane tests) call `Decoder::video`, `Decoder::audio`, `Opened::video`,
`Encoder::video`, `Frame::format`, `Frame::data` and `Frame::data_mut`.

## D12. FFmpeg's log goes to `tracing`

*2026-10-23, M0.*

`spherecut_media::init()` installs an `av_log` callback that sends each
line of FFmpeg's log to `tracing` under the target `ffmpeg`, as one event
that starts with the `[hevc @ 0x...]` prefix of the context that logged
it. FFmpeg formats the text; the prefix is made from the context's
`AVClass` as FFmpeg makes it. FFmpeg may log one line in several calls
(only the first names the context) and several lines in one call, so
each thread keeps the line it has started until a call ends it with
`\n` or `\r`, and gives every line of a call that call's prefix. A line
takes the most severe level among its calls. Error and worse map to
error, warning to warn, info to info, verbose to debug, and FFmpeg's
debug and trace to trace. A panic in the callback is caught and the
message dropped. FFmpeg's own level is set to the most verbose level the
subscriber accepts for `ffmpeg`, so messages nobody would see are never
formatted; a subscriber installed after `init()` calls
`sync_ffmpeg_log_level()`.

The CLI's default filter is `warn,ffmpeg=off`, and `SPHERECUT_LOG` adds
EnvFilter directives to it (README, Logging). FFmpeg's messages are off by
default because doctor's device checks make FFmpeg log errors that are
expected.

Why: before this, `init()` set FFmpeg's level to fatal, so its
explanations were lost (the review's minor on errors and logging). Each
call was later sent as its own event, so "Audio object type 0 is not
implemented" came out in two pieces, the second without its `[aac]`, and
the second line of a message started at column 0 in probe's report.

## D13. What `VideoDecoder` does with a damaged file

*2026-10-23, M0.*

- Only a clean end of file ends the stream with `None`. A read error, a
  cancelled read (`open_cancellable`) or a cut ends it with
  `MediaError::Decode`, after the frames that could still be decoded.
- A cut is found when the file ends before samples that the demuxer's
  index lists for the decoded stream, which is how a moov-first MP4 or MOV
  looks when it is cut. Matroska cuts and most fragmented-MP4 cuts end
  cleanly, with only an FFmpeg warning.
- A packet the demuxer flags corrupt is held back until the next read. If
  that read fails, the packet was part of the failure and is dropped
  rather than decoded.
- Other streams are discarded (`AVDISCARD_ALL`), so damage in them doesn't
  end the decoded one.
- A packet read as invalid data is skipped with a warning and counted
  (`skipped_packets`): the demuxer can resync after one, and later frames
  keep their timestamps. 100 in a row (`MAX_UNREADABLE_IN_A_ROW`) is an
  error.

Why: the M0 review (S4) found that ffmpeg-next's `packets()` iterator
ends on any read error except invalid data, which it skips silently, so an
EIO mid-file looked like a clean end.

Limits: the invalid-data skip has no test, and Matroska and fragmented-MP4
cuts are not detected.

## D14. A hardware decoder says why it refused a stream

*2026-10-23, M0. Extends D3.*

When the device can't decode a stream, `VideoDecoder` returns
`MediaError::HwUnsupported { path, kind, refusal }` with a `HwRefusal`:

- `NotOffered`: FFmpeg never offered the device's pixel format, so this
  FFmpeg has no hardware decoder for the format or profile. This is also
  the answer when no decoder for the codec can use the device at all,
  and when a decoder gave software frames without asking for a format.
- `Withdrawn`: FFmpeg offered it, then setting up its hardware decoder
  failed: the driver can't decode this profile.

The error comes from `open` when no decoder for the codec can use the
device, and otherwise from `next_frame`. Once `next_frame` has returned
it, every later call returns it again and decodes nothing. FFmpeg's HEVC
decoder would otherwise go on in software after the refusal (review
C12).

With a device, `ffi::open_video_decoder` doesn't take FFmpeg's default
decoder for the codec. It takes the first one whose hardware configs
offer `hw_device_ctx` for the device's type. Like `avcodec_find_decoder`,
it takes an experimental decoder only when no other one fits. FFmpeg's
default for AV1 is libdav1d, which has no hardware configs: it ignored
the device and decoded in software (review C13).

`ffi::attach_hw_device` installs the `get_format` callback. In the codec
context's `opaque` (unused by FFmpeg) it keeps the device format in the
low 24 bits, plus flags that describe the latest call. Asked: FFmpeg has
called it at all. Taken: the latest call offered the device format and
we took it. Withdrawn: the latest call lacked the format, and the call
before took it with no device frame since. That is how FFmpeg asks again
when its hardware decoder fails to start. Decoded: a frame has come out
on the device since the format was taken, which `VideoDecoder` notes
(`ffi::note_device_frame`). A list without the format at any other time,
such as after a new SPS, is `NotOffered`, and a call that takes the
format again clears the refusal (review C16). `ffi::hw_refusal` reads
the flags. Doctor's probes (`hwprobe.rs`) use the same callback and
flags. The Tier 2 Vulkan test skips when there is no render node, when
FFmpeg has no Vulkan device or no encoder to make its clip, or on
`HwUnsupported`; any other error fails it.

Limit: a new SPS whose format has no hardware decoder, arriving after
the format was taken and before the first device frame, reads as
`Withdrawn`, not `NotOffered`.

Why: without this the caller got FFmpeg's "Operation not permitted". That
is error code -1, from the `AV_PIX_FMT_NONE` (-1) our callback returns
when it refuses to fall back to software, and it says nothing about the
cause. huginn's first report skipped its Vulkan test with that message.

## D15. MP4 parser limits, damage checks and fuzzing

*2026-10-23, M0. Updates D4.*

Limits, so a hostile file costs bounded time and memory:

- Nesting deeper than 32 levels (`MAX_DEPTH`) is skipped with a warning.
- Parsing stops after 250 000 boxes (`MAX_BOXES`).
- At most 32 warnings are listed; the rest are counted ("N more problems
  not listed").
- A `moov` larger than 256 MiB is not parsed, with a warning.
- Top-level `mdat`, `free`, `skip` and `wide` are never read. Other
  top-level boxes are read only when each is at most 64 KiB and their
  total stays under 1 MiB.
- Leaf payloads over 1 MiB are not kept, except the sample tables (`stts`,
  `stsz`, `stz2`, `stsc`, `stco`, `co64`) and a payload that starts with a
  `tkhd` (a renamed `trak`, below), which the 256 MiB `moov` limit
  bounds.

Damage, which `spherecut probe` fails on (`Mp4Tree::damage()`):

- every warning from the box parser;
- no `moov` box, unless the file is item-based (a top-level `meta`) or the
  cut box may have been `moov`;
- a top-level box cut off by the end of the file, including a file that
  ends inside a box header when the bytes there agree with a header;
- a video sample entry under a `soun` handler, or audio under `vide`;
  other handlers (`pict`, `auxv`) are not checked;
- sample tables that disagree with each other, or chunks outside the file
  or the `mdat` ranges;
- a video or audio `trak` that FFmpeg reads as another kind of stream;
- a box type that is not printable ASCII where only registered types
  belong: in `moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`, the video and
  audio sample entries we know, `sv3d` and `proj`. FFmpeg skips such a box,
  so a `trak` whose type reads `\xd1rak` loses its stream silently. Four
  zero bytes are allowed only as the type of QuickTime's terminator: the
  last box in a sample entry, with nothing but zero bytes after its
  header. Any other box of that type is a header zeroed with the rest of
  its box, which FFmpeg skips with what it holds. An audio `trak` whose
  type, or size and type, were zeroed loses its stream, and an `sv3d`,
  which FFmpeg writes last in the entry, loses the projection. A zeroed
  size makes a box run to the end of its parent, so zeroing the size and
  type of the `st3d` or `colr` before an `sv3d` loses it too. A last box
  over 1 MiB, whose payload is not kept, is not taken for a terminator.
  `udta`, `meta` and `ilst` are not checked, because `©nam` and the like
  are normal there;
- a child of `moov` whose type is printable but not `trak` and whose
  payload starts with a `tkhd` box: a `trak` renamed to `Trak`, ``tr`k``
  or `free`, which FFmpeg skips with its stream just as it skips
  `\xd1rak`. 28 of the 32 one-bit flips of `trak` are still printable.
  FFmpeg-written and Pat's X5 tracks start with `tkhd`. A renamed `mdia`,
  `minf` or `stbl` needs no check of its own, because the track then has
  none, which is damage already. `free` and `skip` count too, although
  they mark space for readers to skip: a track a tool dropped in place by
  retyping it looks the same as a damaged one, so such a file fails
  `probe` and `inject` refuses it, rather than a stream going missing
  without a word. No such file turned up in the sweeps;
- an `sv3d` in a sample entry without `svhd`, without `proj`, or with a
  `proj` that has no `prhd`. FFmpeg then ignores the whole `sv3d`, saying
  no more than "Missing projection box" in its log, so a `proj` renamed
  to `qroj` loses the projection. A projection box FFmpeg does not know,
  such as `mshp`, is not damage;
- a child of a sample entry whose type is printable but not `sv3d` and
  whose payload starts with an `svhd` box, which only `sv3d` holds: an
  `sv3d` renamed to `rv3d` or `sv3D`, which FFmpeg skips with the
  projection, logging nothing. 27 of the 32 one-bit flips of `sv3d` are
  still printable;
- a `mvhd` or `mdhd` timescale of 0, or too large for a signed 32-bit
  number, which FFmpeg replaces with 1, so every time is wrong;
- a video or audio stream FFmpeg could not set up, when its track has
  samples and its sample entry is a codec FFmpeg reads (the entries we
  know, except the encrypted `encv` and `enca`). Like FFmpeg's own check,
  that is no codec, no frame size, no sample rate or channels, or no pixel
  or sample format although FFmpeg has a decoder (the one
  `avcodec_find_decoder` picks for the codec id, read as an int). Usually
  the `avcC` or `hvcC` is damaged. With any other sample entry, such as an
  unknown `zzzz`, or in a track with no samples, where FFmpeg may have had
  too little to go on, the stream is a warning instead: it may be a real
  codec this FFmpeg lacks;
- a video or audio stream whose decoder will not open with its codec
  parameters. Those can look complete when the codec setup is not: the
  MOV demuxer takes the channel count from the sample entry and the AAC
  decoder sets the sample format before it refuses an `esds` with a
  reserved sample rate. So probe opens each such decoder, as a player
  would, on one thread (`avcodec_parameters_to_context` and
  `avcodec_open2` through ffmpeg-next). Opening decodes nothing, and
  probing Pat's X5 files took no measurably longer. A refusal that says
  FFmpeg lacks a feature (`ENOSYS`, `AVERROR_PATCHWELCOME`, or
  `AVERROR_EXPERIMENTAL` for a decoder this build does not enable) is a
  warning, as is a codec with no decoder in this build; any other
  refusal is damage, whatever the container.

A file FFmpeg reads as MOV/MP4 is checked even when its first box type is
unknown; that box alone is a warning, not damage. Matroska is checked
only against its Segment length, other containers not at all, and the
report says so (`structure`). A file with no video or audio stream also
fails `probe`.

What FFmpeg logs at warning or error level while `probe` runs goes into
the report's `ffmpeg_log` (each line once, with a count, at most 32), even
though the CLI's log hides FFmpeg (D12). It often says what FFmpeg could
not read. It is not damage and not a warning, because healthy files can
have some: FFmpeg says "timescale not set" for cover art. When `probe`
fails, the log goes with the error (`ProbeError.ffmpeg_log`), because
for a file FFmpeg cannot open it is often the only explanation ("moov
atom not found"). The CLI prints it under the error, and its JSON error
object is `{"path", "error", "ffmpeg_log"}`.

Not found without decoding: damage in `mdat`, and damage in `moov` that
leaves the structure consistent, such as changed sample sizes or times
(`stts`, `ctts`, `elst`), chunk offsets that still point into `mdat`,
bits of a parameter set, or a box other than those above renamed to
another printable type, such as `edts`, `mvhd`, which leaves FFmpeg
without the movie's timescale and display matrix, or `st3d`, which loses
the stereo mode. Bytes of `mdat` that no chunk uses are not counted as
damage.

Fuzzing, with proptest, in `mp4::tests::fuzz`, under overflow checks:
structure-aware random box trees (containers, `stsd` and sample entries,
`uuid`, 32-bit, 64-bit and size-0 headers) must parse exactly; byte flips
and cuts of them must never panic; damaged and extreme sample tables must
never panic; consistent tables must never be damage; every cut must be an
error or damage. Each parse must take under 1 s and stay within bounds on
boxes, kept bytes and depth. Separate tests nest 20 000 and 200 000 levels
(under 2 s each, under 64 MiB peak growth), make 4000 edits to a `moov`
FFmpeg wrote, of which at least 3000 must be found
(`crates/synth/tests/generated_media.rs`), and run `spherecut probe` on
200 000 levels (`crates/cli/tests/probe.rs`). `write_bad_files` makes one
file for each kind of `moov` damage FFmpeg opens without complaint: chunk
offsets, a `trak` type that is not text and one that still is, an `mdhd`
timescale, a video codec configuration, an AAC setup and a `proj` type.

Why: the M0 review found unbounded recursion and quadratic memory in the
parser (B2), `probe` exiting 0 on corrupt and truncated MP4s (S1), and
that the parser was not fuzzed as D4 said. The final review (C6) found
files that still probed clean: a renamed `trak`, a zero `mdhd` timescale
and a damaged `hvcC`. The review of those fixes found more: an `esds`
whose AAC object type FFmpeg cannot open, which left every field of the
stream set, and printable renames (`Trak`, `qroj`, `wvhd`) that lose a
stream or the projection. The next review found an `sv3d` whose type, or
size and type, were zeroed, which passed as QuickTime's terminator, and
an `sv3d` renamed to `rv3d`.

## D16. `inject` and `Mp4Writer` write atomically

*2026-10-23, M0. Updates D4.*

Both write a temporary file next to the output and rename it into place:

- The name is `.spherecut-<16 random hex digits>.tmp`, so any legal output
  name works. It is created with `create_new`; a taken name is skipped for
  another, up to 16 tries.
- A replaced output keeps its permission bits (the new file is 0600 until
  they are set), but not its owner, ACLs or extended attributes.
- The file is flushed to disk (`sync_all`) before the rename, and removed
  on an error or a panic.
- A symlinked output is written through: the file it finally names is
  replaced and the link kept. A dangling link creates its target.
- Only a regular file at the output is replaced. A directory, pipe,
  device or socket there is refused before any work, and again just
  before the rename: a rename over a pipe or `/dev/null` would replace
  the node instead of writing to it. Writing to a pipe or `/dev/null`
  (say, `render -o /dev/null` for a benchmark) would need its own
  non-atomic path that opens the node and writes it in place.

`inject` also refuses an output that is the input under any name (same
device and inode, so hard links and symlinks too), two edits for one
track, and fragmented MP4 (top-level `moof`, `sidx`, `ssix` or `mfra`, or
`mvex` in `moov`), whose offsets it does not shift. It refuses input that
did not parse cleanly (anything in `Mp4Tree::damage`, which includes the
parser's warnings), because a partial parse can miss the `mdat` after
`moov` and leave every chunk offset unshifted. For the same reason it
refuses when its re-read of the edited `moov` warns, for example when
the edits take it past `MAX_BOXES`. It copies exactly the parsed
lengths, so the output can't outgrow the input.

`Mp4Writer` always writes MP4, through a `StreamIo` on our own file handle,
and looks up both encoders before touching the disk. `finish()` renames the
file into place; `finish_with_edits()` goes through `inject`. Audio input
is planar f32 only, and an encoder that doesn't take it (libopus, for
example) is refused with a clear error.

Why: the M0 review (S2) found that `inject` with the same input and
output grew the file until the disk was full.

## D17. First-order ambiX in AAC uses the 4.0 layout

*2026-10-23, M0.*

Four-channel ambiX audio is encoded as AAC with FFmpeg's default
four-channel layout, 4.0 (channel configuration 4, no PCE). The `SA3D` box
that `inject` adds marks the track as ambiX. In MP4 the muxer writes no
`chnl` or `chan` box for AAC, and `ffprobe` 6.1.1 reports
`channel_layout=4.0`.

Why: FFmpeg's native AAC encoder refuses the ambisonic and unspecified
four-channel layouts (EINVAL, "Unsupported channel layout"; seen on 6.1,
8.1 and 9.0.2). AAC can only name speaker positions. Quad needs a PCE, and
3.1 low-passes the fourth channel as LFE. 4.0 keeps the W, Y, Z, X order,
which the synth writers test checks channel by channel. Google's spatial
audio RFC puts no requirement on the AAC channel configuration.

Cost: FFmpeg's mov demuxer reads our `SA3D` as "ambisonic 1", but
`avformat_find_stream_info` then replaces it with the AAC decoder's 4.0,
so players that ignore `SA3D` play it as 4.0 surround.

Revisit for M2/M6: Opus with channel mapping family 2 (ffprobe 6.1 reports
"ambisonic 1" for it in MP4). It needs packed-float audio in `Mp4Writer`
and a check that YouTube accepts it.

## D18. Doctor checks capability, and reads packages only to word fixes

*2026-10-23, M0. Supersedes part of D8.*

A device that opens can still be useless: Fedora's Mesa VA-API driver
opens with H.264 and HEVC turned off, and FFmpeg's Vulkan device opens on
llvmpipe. So `spherecut doctor`:

- decodes one embedded 256×256 frame each of H.264 High, HEVC Main, HEVC
  Main 10 and AV1 Main in software, on VA-API and on FFmpeg's Vulkan
  device, never falling back to software, and requires the hardware
  picture to match the software decode bit for bit;
- encodes one frame with `h264_vaapi`, `hevc_vaapi` (8 and 10-bit) and
  `av1_vaapi`, and decodes it back;
- lists the Vulkan devices FFmpeg sees (through `AVVulkanDeviceContext`)
  and flags CPU devices as having no GPU acceleration.

Whether something works comes only from those checks, never from which
packages are installed. On Fedora, doctor makes one read-only `rpm -q`
query for a fixed list of packages (RPM Fusion release packages,
`ffmpeg-free`, the Mesa VA-API and Vulkan drivers, the Intel media
driver). The answer only chooses "install" or "swap" wording and whether
RPM Fusion must be enabled first. Doctor never installs or changes
anything.

Fixes are exact `dnf` or `pacman` commands where a package fixes the
problem, on Fedora and Arch. Other problems get the fix that applies
(`usermod -aG render`, GPU passthrough, `MESA_VK_DEVICE_SELECT`), and
other distros get the package named in words. Doctor exits 1 only on an
error: no H.264, HEVC, ProRes or AAC decoder, or nothing that decodes
AV1. `--json` keeps its old keys and adds `gpu`, `vulkan_devices`,
`decode_checks` and `encode_checks`.

`cargo xtask hw-test`'s prerequisite check still runs `rpm -q` for
`ffmpeg-free`, `mesa-va-drivers` and `mesa-va-drivers-freeworld`, and
judges Fedora by those packages.

Why: the M0 review (S6) found that doctor only checked that a device
opened. It caught Fedora's codec-stripped VA-API driver only through an
`rpm -q` guess, so the same problem with Intel drivers or on Arch went
unnoticed, and it reported lavapipe as a working Vulkan device. The review
also found that D8's "never runs package managers" was not true.

## D19. How `cargo xtask hw-test` reports Tier 2 tests

*2026-10-23, M0. Makes D9's "never a pass and never a mystery failure"
hold.*

1. It reads package names from `cargo test --no-run
   --message-format=json-render-diagnostics`.
2. It lists the Tier 2 tests with `cargo test [--release] -- --ignored
   --list`.
3. It runs them with `--no-fail-fast` and `--test-threads=1`; `--quick`
   adds `--skip bench_`.

Every listed test is reported exactly once: passed, FAIL with its output,
skipped with the `TIER2-SKIP:` reason, left out by `--quick`, or did not
run. A test is named `<binary>::<test>`, or `<package>/<binary>::<test>`
when two packages have a test binary of that name. List and run results
are matched by the binary's executable path.

Each test binary's exit is judged on its own, so a skip in one binary
can't hide another's crash. A binary that did not finish, exited
abnormally after its tests finished, or failed with no failing test fails
the run. Before libtest's summary, a bare `ok` printed by a library does
not count as a pass.

hw-test exits 1 when a build prerequisite is missing, the build fails, the
listing fails or finds no test binaries or no Tier 2 tests, a test fails
or does not run, a binary fails as above, or cargo fails for a reason
nothing explains. Skips alone exit 0. The report is written either way.

It runs cargo with `CARGO_TERM_VERBOSE=false`, `CARGO_TERM_QUIET=false`,
`CARGO_TERM_COLOR=never`, `CARGO_TERM_PROGRESS_WHEN=never` and
`RUST_TEST_NOCAPTURE=0`, and passes libtest `--format pretty`, so the
user's cargo settings don't change what it reads. `--samples` is made
absolute, since cargo runs integration tests from the package directory,
and reaches the tests byte for byte (as do `SPHERECUT_BENCH_OUT` and the
other variables), never as a lossy UTF-8 copy. A samples path that is
not valid UTF-8 is used like any other: the samples tests open files
through `probe::probe` and `VideoDecoder::open`, which give FFmpeg the
path's bytes (`ffi::open_input`). The report name gets `-dirty` only for
changes outside `hw-reports/`.

Why: the M0 review found that hw-test silently dropped Tier 2 tests (B1)
and that a relative `--samples` tested nothing while the report said the
samples were present (S5).

## D20. Benchmarks repeat, and report median and spread

*2026-10-23, M0.*

`spherecut_testkit::bench::measure` runs the measurement once to warm up,
then `SPHERECUT_BENCH_RUNS` times (default 3), and records the median,
minimum, maximum and run count. The hw-test report shows Median, Range,
Runs and Spread ((max − min) / median), and marks a spread over 10% as
**unstable**. Details: `docs/perf.md`.

Why: huginn's first report used single runs. VA-API decode of the
synthetic 5760×2880 clip gave 109.96 fps in one run and 67.30 fps in the
next, on an idle GPU.

## D21. The `deny.toml` bans list

*2026-10-23, M0. Updates D8.*

Banned: the HTTP clients `reqwest`, `hyper`, `ureq`, `isahc`, `curl`,
`minreq` and `attohttpc`; the TLS stacks `rustls`, `native-tls` and
`openssl`, all with the reason "no network access at runtime"; the
webview crates `wry`, `tauri` and `webkit2gtk`; and the JavaScript
engines `v8`, `deno_core`, `boa_engine`, `rquickjs`, `quick-js` and
`mozjs`. A JavaScript toolchain (npm and the like) can't come in through
Cargo, so only the engines can be banned here. `multiple-versions` warns,
wildcard versions are denied, and yanked crates are denied.

Revisit if: a dependency we need pulls in one of these without using the
network; add a narrow exception with the reason.

## D22. Direct dependencies

*2026-10-23, M0.*

The heavy ones have their own entries (FFmpeg bindings D2, glam D5). The
rest, with the versions in `Cargo.lock`:

| Crate | Used by | Why |
|---|---|---|
| anyhow 1.0.104 | cli, xtask | Error context at the program edges; libraries use `thiserror` |
| clap 4.6.7 (derive) | cli, xtask | Argument parsing with generated help and errors, for two binaries with subcommands |
| serde 1.0.229 (derive) | core, sphere, media | Serializable time types, `Projection`, and the probe and doctor reports |
| serde_json 1.0.154 | cli | `--json` output. xtask reads cargo's JSON with a small field reader instead |
| thiserror 2.0.21 | core, media, synth | Typed library errors without hand-written `Display` impls |
| tracing 0.1.44 | media, cli | Structured logging, and the target for FFmpeg's log (D12) |
| tracing-core 0.1.36 | media | The `Subscriber` items (`span::Current`, `callsite::rebuild_interest_cache`) that the thread-local FFmpeg log capture of doctor and probe needs. It was already in the tree through `tracing` |
| tracing-subscriber 0.3.23 (env-filter) | cli | EnvFilter gives `SPHERECUT_LOG` its syntax, and the fmt layer writes to stderr. env-filter brings in `matchers` and `regex-automata` |
| proptest 1.12.0 (dev) | core, sphere, media | Property tests and the MP4 fuzzing (D15) |
| ron 0.12.2 (dev) | core | A serde round-trip test of the time types in a text format |

glam's `serde` feature is on, but no glam type is serialized yet.

## Open

- **telemetry-parser** (Gyroflow's camera metadata parser). DJI Osmo 360
  support is only on its git master, and `deny.toml` rejects git sources.
  Options for M4: vendor the parts we need at a pinned commit with their
  license, allow that one git revision in `deny.toml`, or write our own
  parsers from `docs/formats/`. To decide at M4, after checking its license
  and what we actually need from it.
- **Fonts.** egui's default fonts carry OFL-1.1 and Ubuntu Font Licence
  data. Either add a per-crate license exception for
  `epaint_default_fonts`, or ship our own OFL fonts. To decide in M1.
