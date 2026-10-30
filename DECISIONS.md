# Decisions

Each entry says what was decided, why, what else was considered, and what
would make us revisit it. Heavy dependencies are justified here, as the
project rules require. Entries go in number order, with the open
questions last. M1's plan reserved D27 to D37 (`docs/design/m1-plan.md`,
section 0), so each of those lands in its place with its slice, and D38
came before some of them. Superseded entries stay and say so.

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
passes. Rechecked at 2c8d0c0, after the final review's fixes: `cargo +1.88
test --workspace` passes.

*Updated 2026-10-24, M1 slice 3a (the MSRV half of the plan's crate
layout decision):* `rust-version` is **1.95**, from the slice that adds
wgpu and `spherecut-render`. wgpu 30.0.1 itself needs 1.87, but egui 0.37,
which pins it (D7), needs 1.95, so the workspace moves once, with wgpu,
rather than again in slice 7. Fedora 44's RPM is 1.99 (huginn's), not
1.95 as said above, so Fedora still needs no rustup to build. Checked at
4f5f9dd, and again at 2b11aa7 after the slice's first review:
`cargo +1.95 build --workspace --all-targets` builds without warnings and
`cargo +1.95 test --workspace` passes (368 tests, then 377). Rechecked at
0233870, after slice 3b's goldens: both pass, with 389 tests.

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
  too, so they also fail on the 9.0.2 build without Vulkan. Rechecked at
  c2b2753 against that 9.0.2 build: 187 of the 192 Tier 1 tests pass, and
  the 5 that fail are those in `crates/cli/tests/doctor.rs` and
  `hw_refusal.rs`.
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

*Updated 2026-10-24, after the FFmpeg 8 break.* The checks above were
made by hand and nothing made them again. 3a8047d (slice 2a, the
same day) built swscale's flags from `sys::SWS_BILINEAR`,
`SWS_ACCURATE_RND` and `SWS_BITEXACT`. FFmpeg 8 made those defines the
`SwsFlags` enum, which ffmpeg-sys-next 9.0.1 binds as a Rust enum with
no bare constants, so the media crate built here on 6.1 (and 7.1) and
didn't compile on Pat's odin and huginn (FFmpeg 8.1.3, RPM Fusion):
`hw-test` stopped at 09342c6. 67fa1fd takes the flags from ffmpeg-next's
`scaling::Flags`, which names them for each version. Checked since:

- At fc329d1 with 67fa1fd reverted (and again at 69e9496), `cargo xtask
  compat`'s clippy (all targets and features, `-D warnings`) fails
  against 8.1 and 9.0.2 with Pat's three errors and passes against 7.1
  and Ubuntu's 6.1.1.
- Against an 8.1.3 built from its tag with what Pat's FFmpeg has that the
  tests use (VA-API and Vulkan with libdrm, libx264, libx265, libdav1d,
  SVT-AV1, Opus, Vorbis and libshaderc; libavcodec 62.28.103, as on
  odin), the Tier 1 tests at 67fa1fd gave 375 passed, 2 failed and 19
  ignored, where 6.1 passes all 377. Both failures were FFmpeg changing
  between 7.0 and 7.1 (D29). On one thread, as hardware decoders run,
  the HEVC decoder's queue of output frames survived the flush after a
  seek, and a frame from before the seek came out after it, as it would
  on Pat's machines, whose hardware decoders run on one thread on 8.1.3.
  It is fixed, with a test that fails on 8.1.3 without the fix. And the
  HEVC decoder no longer fails a picture it can't decode as invalid
  data, but skips it with a warning; the spliced MP4's second keyframe is
  one, and the same frames come out, so that test now expects the event
  only before 7.1. A third, the stop-token test, failed now and then on
  8.1.3 because its decode was quicker than its 40 ms precondition; it
  writes a longer clip where it needs one. At 1771dc8 every Tier 1 test
  passed against 8.1.3 and 6.1, 390 on each with 19 ignored (the
  one-thread test and compat's 12 unit tests are new), and clippy passed
  against the minimal 7.1, 8.1.3, and the 9.0.2 with VA-API, libx264,
  libx265 and libdav1d.
- The review of that work found what else the 7.1 change does. From 8.0
  the decoder flags as corrupt the pictures that refer to one it skipped,
  and Spherecut dropped every corrupt frame as one decoded from
  references a seek skipped: on 8.1.3 a damaged HEVC reference picture
  lost the rest of its GOP without a `Damaged` event, and an exact seek
  into it returned an earlier frame as `Exact` with nothing said. It is
  fixed (1e73826), with a test that fails on 8.1.3 without the fix. On
  7.1.x the decoder neither returns the error nor flags the frames, so
  there such damage is silent (D29). The review also found that a device
  type an FFmpeg was built without (Vulkan, in that 9.0.2) failed with
  ENOMEM, "Cannot allocate memory", rather than `NotBuiltIn`: FFmpeg knows
  every type's name. Fixed in 78b1753, with a test.
- Tier 1 against a 7.1.3 built the same way (libavcodec 61.19.101;
  Fedora 43 has 7.1) failed three tests. A decoder that fails to open in
  `avformat_find_stream_info` clears the channel layout FFmpeg copies
  back to the stream: 6.1 kept the channels in the old `channels` field,
  which 7.0 removed, and 8.1.3 and 9.0.2 put the sample entry's back. So
  probe read bad-audio-config.mp4 as "has no channels" rather than as
  the AAC decoder's refusal, and an object type the decoder doesn't
  implement became damage rather than a warning. A C program calling
  `avformat_find_stream_info` on the file reads 0 channels against 7.1,
  7.1.3 and 8.1 and 2 against 6.1, 8.1.3 and 9.0.2. probe now asks the
  decoder first (6fabd8b, D15), with a unit test that fails without it,
  and generated_media expected the channels cleared on exactly those
  releases (no longer, below). From 6fabd8b every Tier 1 test passes against 8.1.3, 7.1.3
  and 6.1, 395 on each with 19 ignored, and clippy and the link pass
  against the minimal 7.1 and the 9.0.2 with VA-API, libx264, libx265
  and libdav1d.
- Its review found that asking the decoder first went too far. A
  decoder without `AV_CODEC_CAP_CHANNEL_CONF`, such as PCM's, refuses
  any stream without channels (`EINVAL`), so a `sowt` track whose sample
  entry gives none was damage even with no samples, where it had been a
  warning, and the refusal said no more than "has no channels". Only a
  decoder that sets the channels up itself is asked now (D15).
  generated_media's `a_pcm_track_without_channels_is_damage_only_with_samples`
  writes a PCM MOV and sets its channels to 0, with its samples and
  without, and fails without the fix. The review also found
  generated_media's check of the channels gated on libavformat's
  version, which says nothing of which fixes a point release took
  (FFmpeg's fd6dc624be1e, which puts the channels back, is in 8.1.3 and
  9.0.2 but not 8.1.2 or 9.0.1). It now takes 2 channels or none, which
  are FFmpeg's to say, and checks what is probe's: the AAC decoder's
  refusal as the one damage, and the sample format.
- It found compat itself unsafe. Its targets were named by label alone,
  `<root>/<label>`, and one without a stamp was deleted as stale, before
  a build or under `--clean`: with `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`
  naming the directory that holds the prefixes, each prefix was its own
  target and was deleted. Targets are now `spherecut-compat-<label>`,
  and compat deletes only those with its stamp (below). Its unit tests
  refuse a directory without a stamp, one with a stamp of the old kind
  (no header), a symlink, and a target that is, holds or lies inside a
  prefix, under `--clean` too, and an FFmpeg that changed as its target
  was made ready; each fails without its check. Targets an earlier
  compat made at `compat/<label>` are left as they are: delete them by
  hand.
- It found that compat took a prefix for found once pkg-config read its
  `libavcodec.pc`, though that file's `libdir` or `includedir` could
  lead to another FFmpeg, and that it ran `pkg-config` from `PATH` where
  the build runs `$PKG_CONFIG`. Both are checked now, each with a test
  that fails without its check.
- Its second review found four more gaps. `build-ffmpeg.sh` left a
  prefix whose `make install` failed partway under its final name, with
  libavcodec complete and `libavutil.pc` missing; it now installs into
  `stage-<tag>` beside the prefix and moves the prefix into place only
  once the install is done. compat checked libavcodec alone, though
  ffmpeg-sys-next finds libavutil, libavformat, libswscale,
  libswresample and libavcodec through pkg-config one by one and puts
  libavutil's directory first on the link line, so such a prefix bound
  the system's libavutil; all five are checked now. A target root with
  `..` after a directory not yet made got past the guard and built
  inside the prefix, and a prefix the run didn't name was no guard at
  all; `..` is resolved now, and no target may lie in a directory
  holding FFmpeg's headers. And nothing was checked once the build had
  run, so a prefix that went away during it passed against the system's
  FFmpeg; now the stamp is checked again after it, and so is where the
  build searched for libraries. Each compat check has a unit test that
  fails without it; the staging was checked by hand, with a configure
  and Makefile stubbed to fail at each step.
- The review of the H.264 landing work (D29) found three more in
  compat. Its binding check read ffmpeg-sys-next's build-script output
  from `<target>/debug/build`, where cargo doesn't put it once
  `build.build-dir` or a target triple is set: every build then failed
  that check, and what it bound went unseen. It now reads the
  `build-script-executed` messages cargo prints with
  `--message-format=json-render-diagnostics` (the diagnostics still go
  to stderr as text), from clippy's build as well as the link's,
  wherever cargo ran the script; xtask reads the JSON itself rather than
  take a crate for it. cargo also runs with its build directory set to
  the target, so a `build.build-dir` can't have every FFmpeg's build
  share one, or leave it behind after `--clean`. The libraries compat
  checks were held to the features on the workspace's `ffmpeg-sys-next`
  line by a unit test alone, which a feature turned on anywhere else
  (ffmpeg-next's default features turn on avdevice and avfilter) would
  have passed; every compat run now fails a build that links an FFmpeg
  library pkg-config wasn't asked about, or none of one it was. And the
  module doc said a target inside any FFmpeg's tree is refused, where
  the guard looks only for a directory holding
  `include/libavcodec/avcodec.h` (a source tree has no `include/`); the
  docs say so now, and the error names the directory it found and the
  header file it holds, beside `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`,
  which it named before.
  `what_the_build_bound_is_found_wherever_cargo_builds` builds a
  stand-in ffmpeg-sys-next with a build directory elsewhere and a target
  triple set, and `the_libraries_checked_are_those_the_build_links`
  fails without its check.
- The review of those fixes found three nits more. That check runs only
  in compat, so a feature turned on in the workspace that bound another
  library (ffmpeg-next's `filter`) passed `cargo test` and
  `cargo xtask check` and failed only a compat run:
  `the_libraries_checked_are_those_the_workspace_binds` now reads the
  features cargo metadata resolves for ffmpeg-sys-next, with every
  feature of the workspace on, and fails unless the libraries they bind
  are those compat checks (it fails with `filter` on). libavresample,
  which ffmpeg-sys-next can bind and FFmpeg 5 dropped, didn't count as
  FFmpeg's, so a build that linked one passed; it counts now, as
  libpostproc does. And a target root with a brace in its path failed
  every build, cargo reading its build directory, which compat sets to
  the target, as a template (`{workspace-root}`); such a target is
  refused now, before anything is made there, and the refusal names
  `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`. Each has a test that fails
  without its change.
- `gen-media`'s files from 8.1.3 differ from 6.1's only in FFmpeg's
  version strings (`Lavc`, `Lavf`) and the average bitrate in MP4's
  `btrt` box, which 8.1.3 leaves 0: the streams are the same.

`cargo xtask compat` keeps it checked. For each FFmpeg prefix named, or
listed in `SPHERECUT_FFMPEG_COMPAT` (colon-separated), it runs clippy as
`cargo xtask check` runs it, then builds and links the Tier 1 tests and
the binaries without running them: clippy doesn't link, so a function
that `ffi.rs`'s own extern block declares and an FFmpeg no longer
exports would pass it, and fail only where the tests are built, as
`hw-test` builds them on Pat's machines. Each prefix has a target
directory of its own, `spherecut-compat-<label>` (the label is the
prefix's last component) in the directory
`SPHERECUT_FFMPEG_COMPAT_TARGET_DIR` names, or else in
`<target>/compat/`, with line-tables debuginfo and no incremental
state, and cargo's build directory there too. compat checks that
pkg-config (`$PKG_CONFIG` when it is set, as the build runs it) finds
each library ffmpeg-sys-next binds (libavutil, libavformat, libswscale,
libswresample and libavcodec) in that prefix,
and that the library and header each one's `.pc` file names lie inside
the prefix, symlinks resolved, and prints the FFmpeg and libavcodec it
found there. A target
records what it was built against (the versions, and the size and time
of each library's pkg-config file and of `ffversion.h`), and one built
against anything else is deleted first: ffmpeg-sys-next binds again
only when the variables it reads change, so a distro upgrade of `/usr`,
a prefix built again in place or a symlink pointed elsewhere kept the
old bindings and passed under the new version's name. compat deletes
only what it made: a target, stale or under `--clean`, only while it
holds compat's stamp, whose first line says compat wrote it. A
directory there without one is refused and left, and so is a target
directory that is a prefix, holds one or lies inside one, symlinks and
`..` resolved, or that lies inside any directory holding
`include/libavcodec/avcodec.h`, such as a prefix the run doesn't name
(nothing else of an FFmpeg is looked for: an install with its headers
in Debian's and Ubuntu's `include/<triplet>/` or Fedora's
`include/ffmpeg/` is recognised only if its prefix is listed); the
refusal names
`SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`, which puts the targets elsewhere.
Once a
stale target is deleted, compat asks pkg-config again and stops if the
stamp changed, as it would for a prefix whose files are links into the
target. Once the build has run (the link, and the Tier 1 tests), it
asks again and fails the build if the stamp changed, and reads what
ffmpeg-sys-next's build script did in clippy's build and the link's
from cargo's JSON messages: it must have linked libavcodec and searched
for libraries only inside the prefix, and the FFmpeg libraries the
builds linked must be the five pkg-config was asked about (the report's
"binding"). A prefix moved or deleted during a build had
pkg-config find another FFmpeg for it, which passed under the prefix's
name. cargo
runs without `FFMPEG_DIR`, which ffmpeg-sys-next uses before
pkg-config, and without the pkg-config crate's per-target variables
(`HOST_PKG_CONFIG_PATH`, `PKG_CONFIG_PATH_<target>` and the like), which
it takes over the plain ones. `--test PREFIX` runs the Tier 1 and doc
tests against one, with its libraries on `LD_LIBRARY_PATH`. On this
4-CPU machine a prefix takes about 3 minutes (one for clippy, two for
the link) and 1.4 GB of target, and the Tier 1 and doc tests half a
minute more and 1.8 GB in all; with `--clean` only one target exists at
a time.
`cargo xtask check` runs the clippy and the link when the variable is
set and says it skipped them when not, as it is in a fresh checkout:
only the system FFmpeg is checked then. In a throwaway copy of ec3515e
whose `ffi.rs` declares `avcodec_close` (gone from FFmpeg 8), compat
through a prefix symlinked to 7.1 passes; with the symlink pointed at
8.1 and `FFMPEG_DIR` and `HOST_PKG_CONFIG_PATH` naming 7.1, it deletes
the target, clippy passes and the link fails on the undefined symbol.
`scripts/build-ffmpeg.sh TAG DIR` builds a release from its GitHub tag
the way the 8.1.3 above was built, with Khronos Vulkan headers 1.4.354
where the system's are older than 8.1 needs, as Ubuntu 24.04's are. It
passes their include directory to configure too, which 7.1's configure
needs (it test-compiles `vulkan.h` without the package's Cflags). It
installs into `DIR/stage-TAG` and moves the prefix into place once
`make install` is done, so a failed install leaves no prefix.

*Updated in M1 slice 6.* The audio crate's ALSA bindings broke compat's
link against 8.1.3: alsa-sys's pkg-config puts the system's library
directory (`/usr/lib/x86_64-linux-gnu`) on the linker's search path,
and in playback's test binaries, which depend on audio and media, it
came ahead of ffmpeg-sys-next's prefix, so they linked Ubuntu's FFmpeg
6.1 against 8.1.3's headers and failed on `avcodec_get_supported_config`
(a library with every symbol would have linked without a word, and the
tests run against the wrong FFmpeg). compat now runs every build with
the prefix's library directories as rustc's own `-L` flags
(`CARGO_ENCODED_RUSTFLAGS`, ahead of the caller's flags; rustflags in
cargo's config files don't apply under compat), which rustc searches
before a build script's, and the link fails unless every test
executable and binary loads each FFmpeg library it needs from the
prefix, as `ldd` resolves them with the prefix's libraries on the
loader's path. Its test runs the check on xtask's own test binary,
which links the FFmpeg it was built against: it passes for `/` and
fails for an empty directory, naming libavcodec.

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

*Updated 2026-10-24 by D30:* the backend's `Fallback::Software` is that
fallback, with a warning and a `FellBack` event before the next frame;
any other failure on the device is `MediaError::HwFailed`, never damage.

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

*Updated by D27:* `Rational` serializes as the text `"n/d"` (`"n"` for an
integer) and parses from it with `FromStr`, `Time` as `{"exact",
"seconds"}`, and `FrameTime` (a `Time` and, when known, its frame index)
joins them.
ron used to write a frame rate as `(60000,1001)`; it now writes
`"60000/1001"`, and the old form is refused.

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

*Updated by D25 and D26:* zero-copy import of VA-API frames is proved in
M1 and decided before M3, and egui is checked against a timeline
prototype at the end of M2, with Qt/QML as the fallback.

*Updated by D31:* 10-bit planes upload as `R16Uint` or `Rg16Uint` and
an ingest pass turns them into `R16Float` or `Rg16Float`, since 16-bit
normalized formats aren't renderable without optional features and the
mip pass renders; 8-bit planes stay `R8Unorm` and `Rg8Unorm`. wgpu is
built with only its Vulkan backend and WGSL front end.

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

*Updated by D23:* `deny.toml` covers Rust crates only, not what the
system FFmpeg can do. M1 shuts FFmpeg's network protocols and nested
opens, and bans `std::net`'s TCP and UDP types with clippy.

*Updated by D24*, which does that: FFmpeg gets only the `file` protocol,
the MP4/MOV and Matroska demuxers and no nested opens, clippy forbids
`std::net`'s TCP and UDP types, and Tier 1 tests run probe and doctor
under strace and fail on any `AF_INET` or `AF_INET6` socket.

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
  output (sha256 over all 19 files). Rechecked 2026-10-23 at 6b309fb: two
  more runs gave byte-identical output over all 22 files (12 clips, 9 bad
  files and `MANIFEST.md`), the same bytes as `test-media/`.
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

*Updated 2026-10-24 by D29:* a seek clears a read failure, a cut and the
end of the stream, so a seek back into the good part of a truncated file
decodes again; a hardware refusal stays. The cancel flag is for shutdown
only: seeks stop through generation tokens instead.

*Updated 2026-10-24, against FFmpeg 8.1.3 (D2, D29):* from 7.1 the HEVC
decoder skips a picture whose slice header or frame start it can't
decode without returning an error, and from 8.0 it treats the pictures
that refer to it as damaged, where 6.1 decoded them from what it had.
`VideoDecoder::open` doesn't ask for corrupt frames
(`AV_CODEC_FLAG_OUTPUT_CORRUPT`), and without that flag 8.x refuses
those pictures: a reference picture that is missing or corrupt is
invalid data to `add_candidate_ref`, `hevc_frame_start` drops the
picture, and `decode_nal_unit` skips it with the same "Skipping invalid
undecodable NALU" warning, up to the next IRAP picture. So there such
damage loses those frames and `next_frame` returns no error. The
backend's decoder asks for corrupt frames, so FFmpeg decodes those
pictures and flags them corrupt, and for HEVC the backend says the first
of them after a good frame as a `Damaged` event and drops the rest
(D29). H.264's flag says only where decoding recovered, not damage, and
the backend hands those frames out, and takes the I-frame decoding
starts from for good, flagged or not (D29). On 7.1.x the HEVC damage is
silent on both.

*Updated 2026-10-28, M1 slice 4's review:* a Matroska file shorter
than its Segment says is cut. `VideoDecoder` reads the Segment's start
and size as probe does (`probe::matroska_segment_of`), and a file that
ends before the Segment's end ends the stream with the cut, so a seek
or a count past a Matroska cut is `Truncated`, never the last frame
returned as the one asked for. Still not detected: a Matroska file
whose Segment's size is unknown (a live recording), and a fragmented
MP4 cut between fragments, which reads as a shorter file. The
backend's `next_frame` also says a failed read as a `Damaged` event
with its cause, once: before the first frame decoded after it, or
before the error when none is (D29), so that a caller that only plays
on learns where the damage was and why. Tested in
`crates/synth/tests/seek.rs` (a cut clip, as H.264 and HEVC MP4 and as
Matroska) and `crates/cli/tests/frame.rs` (frames past a Matroska cut,
and a frame a cut lost, are refused).

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
`HwUnsupported`; any other error fails it. *Updated by D38:* the Vulkan
tests no longer need a render node, run on the GPU behind it if there is
one, skip when FFmpeg's only Vulkan devices run on the CPU, and give the
device and FFmpeg's own words when they skip.

*Updated 2026-10-24 by D30:* through the backend, a refusal at open
falls back as `Refused(refusal)`, and one after frames came out as
`Failed`; any other error on the device is `HwFailed`, sticky in the same
way.

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

- every warning from the box parser. One is a box below the top level
  whose 32-bit size is 0, unless it and everything after it in its parent
  are zero bytes. ISO/IEC 14496-12 and QuickTime allow size 0 only for the
  last top-level box. Below that it runs the box to the end of its parent,
  so FFmpeg reads the boxes after it as its payload: a zeroed `hvcC` or
  `st3d` size loses the `sv3d` after it, an `stsd` size the sample
  tables, a `trak` size the tracks after it, all with exit 0 before. A
  parent's last box with size 0 loses nothing, but it breaks the same
  rule, and none of the healthy files in the sweeps has one, so it counts
  too. Only boxes the parser reads are checked: `moov`, and the small
  top-level containers it loads, such as `udta` and `meta`. Fragments
  (`moof`, `traf`) are not read;
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
  belong: in `moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`, `edts`,
  `dinf`, `mvex`, `tref`, the video and audio sample entries we know,
  `sv3d` and `proj`. FFmpeg skips such a box, so a `trak` whose type
  reads `\xd1rak` loses its stream silently, and a zeroed `elst` moves the
  track's start time. Four zero bytes are allowed only as the type of
  QuickTime's terminator: the last box in a sample entry, with nothing but
  zero bytes after its header. `inject` keeps a terminator last when it
  adds boxes to an entry: that is the only place it is allowed, and as
  eight zero bytes it has size 0 and hides what follows from FFmpeg. Any
  other box of that type is a header zeroed with the rest of
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
  refusal is damage, whatever the container. A decoder that sets the
  channels up itself (`AV_CODEC_CAP_CHANNEL_CONF`, as AAC's does) and
  fails to open in `avformat_find_stream_info` also clears the stream's
  channels from FFmpeg 7.0, until 8.1.3 and 9.0.2 put the sample entry's
  back (6.1 kept them in the old `channels` field), so an audio stream
  with no channels is judged by such a decoder first: only one that
  opens leaves "has no channels" as the reason (D2). Any other decoder,
  such as PCM's, refuses a stream without channels (`EINVAL`), which
  says only that it has none, so it isn't asked: "has no channels" is
  the reason, and damage only with samples, as above.

A file FFmpeg reads as MOV/MP4 is checked even when its first box type is
unknown; that box alone is a warning, not damage. Matroska is checked
only against its Segment length, and the report says so (`structure`).
A file with no video or audio stream also fails `probe`.

*Updated by D24:* other containers used to be checked not at all, with
`structure: none`. Now they aren't opened: `probe` fails on them with
the container's name, before FFmpeg's demuxer for it runs.

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

*Updated by D23:* the CLI will refuse an existing output unless
`--overwrite` is given (PLAN M1), and `spherecut mcp` refuses an output
whose last component is a symlink. The write-through above then applies
only to the library and to the CLI with `--overwrite`.

*Updated in M1 slice 4*, for `spherecut frame`, the first command that
writes:

- The temporary file is `spherecut_core::fs::TempFile`, moved from
  `inject`, which uses it as before, as `Mp4Writer` does. It publishes
  two ways. `persist_replace` is the rename above. `persist_new`
  publishes only if nothing is at the output, not even a dangling link:
  it flushes the file, then `link(2)`s it to the output's name, which
  fails with `EEXIST` whatever turned up there since the check, and
  removes the temporary name. That error is `AlreadyExists` and leaves
  what is there alone. On a filesystem without hard links (`EPERM` on
  FAT, `EOPNOTSUPP` or `ENOSYS` on some network filesystems; any other
  refusal of the link too) it creates the output with `create_new`
  (`O_CREAT | O_EXCL`), copies the file into it and flushes it, and
  removes the output again if the copy fails. The link call is a
  parameter of a private twin of `persist_new`, so a unit test makes it
  refuse. A temporary file it couldn't remove is a `tracing` warning,
  and the copy a debug line (D22).
- **frame without `--overwrite`** refuses anything at the output before
  any work, before probe even: a regular file or a link to one (even a
  dangling link, which the library would write through) is
  `output_exists`; a directory, pipe, socket or device there, or a link
  to one, is `not_a_file` (exit 1 both), and so is a name that can only
  be a directory's (ending in `/` or `/.`), with or without
  `--overwrite`, whether or not it exists. It then writes next to the
  output and publishes with `persist_new`, so a file that appears at the output while the frame
  renders is kept, and the run fails with `output_exists` saying so.
  Debug builds have a switch, `SPHERECUT_DEBUG_FRAME_RACE`, that writes
  the output just before the publish, which `crates/cli/tests/frame.rs`
  uses to lose that race on purpose; release builds don't have it.
- **With `--overwrite`**, links are followed as above, a regular file is
  replaced by `persist_replace`, and anything else is `not_a_file`, as
  before.
- A failed or interrupted run leaves neither an output nor a temporary
  file: the first Ctrl+C stops the work and the temporary file is
  dropped (D27). Only a second Ctrl+C, which kills the process at once,
  can leave a `.spherecut-….tmp` beside the output. That holds from the
  first byte: frame installs its SIGINT handler after the output checks
  and before it makes the temporary file (slice 4's review found it the
  other way round, leaving a moment in which the first Ctrl+C killed
  the process with the file made).

Why the link: a check and then a rename can't refuse an existing output
atomically, since a file can appear between them and the rename would
replace it. `link(2)` fails if the name exists, never replaces it, and
gives the name to a file that is already whole. `renameat2` with
`RENAME_NOREPLACE` would do the same, but std doesn't wrap it, and core
forbids `unsafe` (D28). Why frame doesn't write through a link without
`--overwrite`: a link at the output is something there, and writing
through it replaces the file it names, which is what `--overwrite`
asks for.

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
other distros get the package named in words. A fix is lines for people:
a command may be followed by three spaces and a note (`sudo dnf install
mesa-va-drivers-freeworld   (RPM Fusion free)`), so the command is what
comes before them, and a line with no command is a sentence
(`docs/cli-json.md`, D27). Doctor exits 1 only on an
error: no H.264, HEVC, ProRes or AAC decoder, or nothing that decodes
AV1. `--json` keeps its old keys and adds `gpu`, `vulkan_devices`,
`decode_checks` and `encode_checks`.

Every decoder and encoder in doctor's tables that this FFmpeg lacks gets
its own issue, naming the codec, with the FFmpeg fix, except that an
FFmpeg built without VA-API gets one issue for that instead of one per
VA-API encoder. Each failed check has a fault: `ffmpeg` (the build left it
out, or FFmpeg's own software decode failed or came out in the wrong
format), `driver` (the driver or hardware refuses it) or `wrong_output`
(it runs and the picture or the read-back differs). A refusal, by the
build or by the driver, falls back to the CPU (D3); wrong output does not.
A driver with wrong output gives no error, so doctor says its wrong
pictures would be used and suggests a newer driver. That makes VA-API
wrong output a warning, and Vulkan Video wrong output a warning when
VA-API can't decode at least one of those formats, since Vulkan's pictures
are then the ones used. Otherwise a Vulkan Video decode problem is a note,
because VA-API is the first hardware decoder.

`cargo xtask hw-test`'s prerequisite check still runs `rpm -q` for
`ffmpeg-free`, `mesa-va-drivers` and `mesa-va-drivers-freeworld`, and
judges Fedora by those packages.

Why: the M0 review (S6) found that doctor only checked that a device
opened. It caught Fedora's codec-stripped VA-API driver only through an
`rpm -q` guess, so the same problem with Intel drivers or on Arch went
unnoticed, and it reported lavapipe as a working Vulkan device. The review
also found that D8's "never runs package managers" was not true.

*Updated by D27:* `doctor --json` prints its report, with the keys
above, as the `result` of a `spherecut.doctor/1` document. An issue of
severity error makes `ok` false, with the code `doctor_errors`, and still
exits 1; a command line that doesn't parse exits 2.

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

*Updated by D37* (M1 slice 5): the report gains "Power profile",
"Render nodes", "Perf clips" and "Tier 1 on this machine", and a Tier 1
failure or a perf clip that can't be made fails the run. `--list`
prints the Tier 2 tests and stops.

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

*Updated by D37* (M1 slice 5): a row named "… per second" is one run's
seconds (the playback benchmarks), with Runs counting the seconds and
Range the slowest and the fastest; its spread says how even playback
was, not how busy the machine was.

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

*Updated by D23:* a crate can add a network transport without pulling a
banned crate (rmcp's HTTP server feature is one), so a crate with such
features gets a `[[bans.features]]` allow list when it is added.

*Updated by D36:* `[[bans.features]]` also keeps eframe's `accesskit`
and `links`, winit's `wayland-csd-adwaita` and egui_kittest's `eframe`
off, and the graph is Linux's only.

## D22. Direct dependencies

*2026-10-23, M0.*

The heavy ones have their own entries (FFmpeg bindings D2, glam D5). The
rest, with the versions in `Cargo.lock`:

| Crate | Used by | Why |
|---|---|---|
| anyhow 1.0.104 | cli, xtask | Error context at the program edges; libraries use `thiserror` |
| ash 0.38.0 (std) | render | Vulkan's own calls that wgpu-hal doesn't make (slice 5b): in `render::dmabuf`, asking the driver which DRM format modifiers it has for a format and whether it imports an image with one from a DMA-BUF, and how large (`vkGetPhysicalDeviceFormatProperties2`, `vkGetPhysicalDeviceImageFormatProperties2`), through the instance wgpu-hal opened; in `render::vkshare` (path B), the shared device's extensions, features and queues, and the copier's command buffers, barriers, image-to-buffer copies, timeline semaphore and buffers. Pinned with `=` to the version wgpu-hal 30.0.1 uses, so that its types are wgpu-hal's; default features off, so no loader of its own. Already in the tree through wgpu-hal, so it adds no crate |
| bytemuck 1.25.2 (derive) | render | The `#[repr(C)]` uniforms as bytes for `Queue::write_buffer`, and timestamps read back, with no `unsafe`: its derives compile under `forbid(unsafe_code)` (D31) |
| clap 4.6.7 (derive) | cli, xtask | Argument parsing with generated help and errors, for two binaries with subcommands |
| cpal 0.18.2 (pulseaudio) | audio | The audio output (D35): the PulseAudio protocol host, pure Rust, which reaches PipeWire through pipewire-pulse, and ALSA, which cpal always builds on Linux and which links libasound (so its headers are a build prerequisite). Default features off; a `[[bans.features]]` allow list keeps `pipewire`, `jack`, `realtime` and the browser hosts off. On Linux it brings `alsa`, `alsa-sys`, `pulseaudio` and its `byteorder`, `enum-primitive-derive`, `mio`, `thiserror` 1 and the `futures` crates, and `dasp_sample` for its sample conversions |
| eframe 0.37.0 (wgpu_no_default_features, wayland, x11, default_fonts, persistence, clipboard) | app | The GUI's window, event loop and persistence (D7, D36), on render's own device through egui-wgpu's `WgpuSetup::Existing` (D31). Default features off: no `accesskit` (zbus advisories), `links`, `system_fonts` or `glow`, which `[[bans.features]]` keep off. It brings egui-winit, winit, arboard and smithay-clipboard (the clipboard), ron for its storage, and the Wayland and X11 client crates |
| egui 0.37.0 | app | The immediate-mode UI (D7, D26). Its own `accesskit` dependency is the data types only, with no zbus. It brings epaint, emath, ecolor, epaint_default_fonts (the fonts' licence exception, D36) and, inside epaint, vello_cpu, skrifa and harfrust for text |
| egui-wgpu 0.37.0 | app | egui's renderer on wgpu, and the paint callback the viewer draws through (D36). Default features off |
| egui_tiles 0.18.0 | app | The tiles: the viewer beside the Info, Doctor and Files tabs, saved by eframe's persistence (D7: egui_dock has no release for egui 0.37) |
| image 0.25.10 (png only) | render; app (dev) | `Image::to_png` and `from_png`: 8- and 16-bit RGB PNGs for `spherecut frame` and the goldens. Its default features are off, so it brings no other codec and no rayon, and a `[[bans.features]]` allow list in `deny.toml` keeps it that way. It pulls `png`, `fdeflate`, `flate2`, `miniz_oxide`, `crc32fast`, `simd-adler32` and `adler2` for PNG, and, unconditionally, `moxcms` and `pxfm` (its colour-management code and the maths under it, BSD-3-Clause OR Apache-2.0), `num-traits` and `byteorder-lite` |
| libc 0.2.191 | cli, media | `sigaction(2)` for `spherecut frame`'s SIGINT handler, in `cli::interrupt`, the binary's one FFI module (D28): the handler only stores to an atomic. In media's `ffi`, `mmap`, `madvise` and `munmap` for the download pool's buffers on huge pages (D30). It was already in the tree through ffmpeg-sys-next, ffmpeg-next, wgpu-hal and parking_lot_core, so it adds no crate |
| pollster 1.0.1 | render | Blocks on wgpu's few futures (enumerating adapters, the device, error scopes) in headless code, without an async runtime |
| serde 1.0.229 (derive) | core, sphere, media, cli | Serializable time types, `Projection`, the probe and doctor reports, and the CLI's JSON envelope (D27) |
| serde_json 1.0.154 | cli; core, render (dev) | `--json` output, through the `contract` module (D27). core's tests round-trip `ColorInfo` through JSON as the CLI will write it, and render's goldens read and write their manifest (D32). xtask reads cargo's JSON with a small field reader instead |
| thiserror 2.0.21 | core, media, synth, render, playback, audio | Typed library errors without hand-written `Display` impls |
| tracing 0.1.44 | core, media, cli, render, playback, audio | Structured logging, and the target for FFmpeg's log (D12). core logs only what `fs::TempFile` can't clean up (M1 slice 4) |
| tracing-core 0.1.36 | media | The `Subscriber` items (`span::Current`, `callsite::rebuild_interest_cache`) that the thread-local FFmpeg log capture of doctor and probe needs. It was already in the tree through `tracing` |
| tracing-subscriber 0.3.23 (env-filter) | cli | EnvFilter gives `SPHERECUT_LOG` its syntax, and the fmt layer writes to stderr. env-filter brings in `matchers` and `regex-automata` |
| wgpu 30.0.1 (std, parking_lot, vulkan, wgsl) | render | The GPU (D7, D31). Default features off, so only the Vulkan backend and the WGSL front end are built: no GL, Metal, DX12 or WebGPU backend, no SPIR-V or GLSL front end. `std` and `parking_lot` are the two defaults kept: the standard library, and wgpu's locks. A `[[bans.features]]` list in `deny.toml` refuses `web`, `webgpu` and `webgl` (wasm-bindgen, js-sys and web-sys), `angle`, `static-dxc`, `glsl`, `spirv` and `vulkan-portability` if a later crate turns them on; GL, Metal and DX12 aren't banned, since egui_kittest turns them on in test builds (m1-plan section 11). It brings wgpu-core, wgpu-hal, wgpu-types, naga, ash and gpu-allocator among others. With image, bytemuck and pollster, slice 3a adds 57 crates to what a Linux build compiles (`cargo tree --target x86_64-unknown-linux-gnu` on normal and build edges, against ef1a9d1; 56 on normal edges alone), all under the existing licence list; `Cargo.lock` gains 90, the rest for other platforms' backends, which Linux never builds. 3a first said 46, a miscount the first review found |
| winit 0.30.13 (rwh_06, wayland, wayland-dlopen, x11) | app | Not called: named so that eframe's winit gets `wayland-dlopen` back by feature unification, and libwayland is loaded at run time (D36). Never `wayland-csd-adwaita` (RUSTSEC-2026-0192 through ttf-parser), which `[[bans.features]]` refuses |
| naga 30.0.1 (wgsl-in) (dev) | render | The test that each uniform's Rust layout matches its WGSL struct, member by member, which a driver doesn't check (D31). The same naga wgpu builds, so it adds nothing to the tree |
| egui_kittest 0.37.0 (wgpu) (dev) | app | The GUI's tests on lavapipe, through our device with `WgpuTestRenderer::from_render_state` (D36), and its accessibility tree for finding widgets. Not its `eframe` feature, which turns eframe's accesskit on and `[[bans.features]]` refuses. In test builds it turns on wgpu's `gles`, `metal` and `dx12` (D31's row); the shipped binary has none |
| proptest 1.12.0 (dev) | core, sphere, media, render, playback | Property tests and the MP4 fuzzing (D15) |
| ron 0.12.2 (dev) | core | A serde round-trip test of the time types in a text format |

glam's `serde` feature is on, but no glam type is serialized yet.

*Updated in M1 slice 3a,* with the render crate's rows above: cargo-deny
now warns of three more crates in two versions, each inside the new
trees: hashbrown 0.16 (gpu-allocator's) beside 0.17, rustc-hash 1 (naga's)
beside 2, and miniz_oxide 0.8 beside 0.9, both within png. Duplicates
warn rather than fail (D21), and none can be unified from here.

*Updated in M1 slice 5,* with the playback crate: it depends on core,
media and thiserror, and on proptest for its tests, all in the tree
already, so it adds no crate. core's tracing now also logs the first
blocking call on a release build's UI thread (D34). The engine, the
slice's second part, adds render and tracing to playback's
dependencies, and synth and media's `fault-injection` to its tests'.
They are all in the tree already, so it adds no crate either.
The benchmarks, the third part, add sphere and testkit to its tests'
dependencies, and hw-test's perf clips use synth, which xtask had.
Neither adds a crate.

*Updated in M1 slice 6,* with the audio crate (cpal's row above). It
depends on core, cpal, thiserror and tracing, and on testkit for its
Tier 2 test; playback depends on it. It adds 20 crates to what a Linux build
compiles (`cargo tree --target x86_64-unknown-linux-gnu` on normal and
build edges, against 43abcda): `cpal`, `alsa`, `alsa-sys`, `dasp_sample`,
`pulseaudio`, `byteorder`, `enum-primitive-derive`, `mio`, `slab`,
`thiserror` and `thiserror-impl` 1, and nine `futures` crates, all under
the existing licence list. `Cargo.lock` gains 44, the rest for other
platforms' hosts (Android's `ndk` and `jni`, CoreAudio, WASAPI), which
Linux never builds. cargo-deny warns of two more duplicates: thiserror 1
(pulseaudio's) beside 2, and jni-sys 0.3 beside 0.4, both within cpal's
Android tree.

*Updated in M1 slice 7,* with the app crate (the eframe, egui,
egui-wgpu, egui_tiles, winit and egui_kittest rows above). It adds 90
crates to what a Linux build compiles (`cargo tree --target
x86_64-unknown-linux-gnu` on normal and build edges, against 303e364:
142 to 232; the same 90 on normal edges alone), close to the plan's
estimate of about 220 on normal edges (217), all under the existing
licence list but the fonts' (D36). `Cargo.lock` gains 177, the rest for
other platforms' windowing and clipboards. `deny.toml`'s graph is now
Linux's alone (D36), so the duplicates it warns of are Linux's: the GUI
adds calloop 0.13 beside 0.14, calloop-wayland-source 0.3 beside 0.4 and
smithay-client-toolkit 0.19 beside 0.20 (winit's and smithay-clipboard's
Wayland stacks), rustix 0.38 beside 1 and linux-raw-sys 0.4 beside
0.12, nom 7 beside 8, foldhash 0.1 beside 0.2, and a third hashbrown
(0.15); jni-sys's pair, Android's, no longer counts. The cli depends on
app and playback; app on audio, core, media, playback, render, sphere,
glam, serde, thiserror, tracing and wgpu, and on synth and media's
`fault-injection` for its tests.

*Updated in M1 slice 5b,* with ash's row above, which adds no crate: it
was wgpu-hal's already, at the same version.

## D23. Agent integration: an MCP server and the JSON CLI

*2026-10-24, before M1's code. Pat asked for agent integration so that
"all the agents" can use Spherecut. PLAN.md track A has the criteria.*

What: `spherecut mcp`, a Model Context Protocol server on stdin/stdout,
plus a JSON contract for the CLI and `spherecut call <tool> '<json>'`, all
on one set of request and response types. Read-only by default, with the
read and write roots enforced by the kernel (`openat2` with
`RESOLVE_BENEATH`). A few general tools (`probe`, `look`, `analyze`,
`check`, `doctor`, then `project`, `edit`, `undo`, `redo`, `render`,
`job`, `compare`); later features add edits and fields, not tools.

Why MCP: every agent checked on 2026-10-24 can start a local stdio MCP
server (Claude Code, Codex, Gemini CLI, Copilot CLI, Cursor, Zed, goose,
opencode), so one server reaches all of them without vendor code. stdio
means no port. The CLI path covers agents that only run commands; they
look at frames through PNG files.

What the research found (each fact with its source in
`docs/research/agents-2026-10.md`), which shapes PLAN's criteria:

- MCP has split. Revision 2026-07-28 drops the `initialize` handshake
  (each request carries its version in `_meta`, servers implement
  `server/discover`), and a client that sends `initialize` fails against
  a server that only speaks 2026-07-28. Codex's stdio client still uses
  the handshake by default, Gemini CLI's SDK (1.23.0) tops out at
  2025-06-18, and Claude Code asks for 2026-07-28 and falls back. So the
  server serves 2025-06-18 and 2025-11-25 through the handshake, which
  every client checked can use, and 2026-07-28 too if the MCP layer
  chosen provides it (rmcp does; our own layer would add it when a
  client needs it). Under the stateless revision, state that spans calls
  must be explicit, hence project tools that name the project file.
- Clients cut long calls off: Codex after 60 s by default, Claude Code at
  a per-server wall-clock limit that progress doesn't extend (and it
  moves a call to the background after 2 min), Gemini CLI after 10 min.
  So `render` returns a job at once.
- Images count against Claude Code's 25,000-token tool output limit, and
  a request with over 20 images rejects any over 2000 px. Claude bills
  ceil(w/28) × ceil(h/28) visual tokens, so twelve 768×432 frames cost
  about four times one 12-tile contact sheet. Codex replaces images with
  a placeholder for models without image input. Hence small JPEG
  defaults, contact sheets, and text results that name every tile.
- MCP roots are deprecated and were never access control, so the roots
  are server options. Sampling is deprecated and nothing here needs it.
- Agent Skills (`SKILL.md`) is a standard that Claude Code, Codex, Gemini
  CLI and Copilot CLI all read, from `~/.claude/skills` and
  `~/.agents/skills`. `AGENTS.md` is for agents working on a repository,
  so it isn't used for this.
- The official conformance suite and MCP Inspector run on npm, which the
  prompt rules out, so Spherecut tests its server with its own client.

Not a plugin API: the prompt puts a plugin API out of scope, and this
stays clear of what makes one: no outside code runs in Spherecut, and
tools take data only. It is a public interface, though, so the schemas
are versioned and their changes listed in `docs/agents.md`.

Network: Spherecut makes no network requests, but what a tool returns
goes to the agent's model provider, which `docs/agents.md` says first.
A3's attach socket is local IPC, not network access; it is opt-in, a path
socket with mode 0600, and peers are checked with `SO_PEERCRED`. Pat
confirms that reading before A3.

*Updates D8 and D21.* The review found that `deny.toml` can't see what
FFmpeg does, since FFmpeg is a system C library: M0's probe follows an
HLS playlist to a file outside its folder, and a playlist with `http://`
entries would make FFmpeg use the network. M1 sets `protocol_whitelist`
to `file`, limits the demuxers, refuses nested opens through `io_open`
and adds a clippy ban on `std::net`'s TCP and UDP types, with Tier 1
tests including an `strace` check for network sockets.

*Updated by D24*, which does that, and gives every decoder a
`max_pixels` limit (`MAX_PIXELS`, one of the limits PLAN.md track A
lists). `spherecut frame`'s strace test comes with `frame`.

*Updated by D27*, the JSON contract of the CLI for probe and doctor. Its
envelope, codes and time shapes are in `spherecut_cli::contract` and
`spherecut_core`, for A1's tools to return.

To decide at A1, with a prototype of each:

- **MCP layer.** rmcp 3.7.0 (the official Rust SDK, Apache-2.0, MSRV
  1.88) serves both sides of the split, and Codex's own client uses it.
  It needs tokio: `default-features = false, features = ["server"]` with
  our own tokio stdin/stdout adds 23 crates, none banned, and `cargo
  deny` passes. Its HTTP server feature would pass `deny.toml` unnoticed,
  so it would come with a `[[bans.features]]` allow list for rmcp, which
  was tested to catch that. It has had 15 releases since 2026-07-28, so
  it would be pinned with `=`. The other choice is our own synchronous
  JSON-RPC layer on `serde_json` and std threads, about 800 to 1,500
  lines plus tests (a reviewer's estimate), with no async runtime, at the
  cost of tracking each MCP revision ourselves.
- **Schemas** from the Rust types with schemars 1.x (MIT; dyn-clone,
  ref-cast, serde and serde_json, plus schemars_derive and
  serde_derive_internals for the derive), or written by hand. Either way
  they are checked into `docs/agents/schemas/` and a test compares them
  with what the server advertises.
- **openat2 and `SO_PEERCRED`**, which std doesn't offer: rustix as a
  normal dependency (safe wrappers; its openat2 to be checked first), or
  `libc` calls in an FFI module with SAFETY comments, since `unsafe` is
  allowed only in FFI and GPU modules.

Revisit if: a new MCP revision changes the split, or agents converge on
something other than MCP.

## D24. FFmpeg opens only the file Spherecut opened

*2026-10-24, M1. Updates D8, D15 and D23. The criterion is in PLAN.md
M1.*

What: every file Spherecut demuxes is opened by `ffi::open_input`, which
holds FFmpeg to that one file:

- The path must be a regular file. FFmpeg gets `file:` followed by the
  path's bytes, and its file protocol strips the `file:` and opens the
  rest as it is. So `http://127.0.0.1:1/x.mp4` is the path
  `http:/127.0.0.1:1/x.mp4`, and a file named `concat:x.mp4`,
  `subfile,,start,0,end,0,,:x.mp4` or `file:x.mp4` is read as that file.
  FFmpeg took such names for URLs when no directory came before them:
  M0's probe of `concat:x.mp4` read `x.mp4`.
- The demuxer is identified first, with `av_probe_input_buffer2` (the
  function `avformat_open_input` uses) on an `avio_open2` context
  limited to the file protocol. It must be one of `ffi::DEMUXERS`,
  FFmpeg's `mov,mp4,m4a,3gp,3g2,mj2` and `matroska,webm`, which hold
  every camera format in `docs/formats/` (`.insv`, `.lrv`, `.360`,
  `.OSV`, `.LRF` and `.mp4` are all MP4). Any other is refused before its
  demuxer reads anything, with `MediaError::UnsupportedContainer`, which
  names the container as FFmpeg does: "FFmpeg reads list.m3u8 as Apple
  HTTP Live Streaming (`hls`), and Spherecut opens only MP4/MOV and
  Matroska/WebM files". The file is then opened with that demuxer given
  explicitly, so a file swapped between the two opens is still read only
  by an allowed demuxer.
- The format context gets `protocol_whitelist` `file`, a
  `format_whitelist` of the two demuxers, and `enable_drefs` 0 for MOV.
  0 is FFmpeg's default, set here so that a MOV file's references to
  external data are never followed.
- Its `io_open` is `open_only_the_file`. That passes FFmpeg's own
  `io_open` only the open that gives the context its input: for reading,
  into `s->pb` while that is NULL, of the context's own URL, as
  `avformat_open_input` does in FFmpeg 6.1, 8.1 and 9.0. Every other open
  is refused with EPERM and a `tracing` warning, and a refusal while the
  file is being opened fails the open with the URL FFmpeg asked for
  (`MediaError::Open`). `io_close2`, and FFmpeg 6.1's deprecated
  `io_close`, stay FFmpeg's own: the only AVIOContext is the one
  FFmpeg's `io_open` opened.
- Every decoder gets `max_pixels` = `MAX_PIXELS` = 2^27 (16384 × 8192):
  those `avformat_find_stream_info` opens, through its per-stream options,
  and those Spherecut opens, all through `ffi::open_decoder` (`probe`,
  `VideoDecoder`, doctor's probes). That is twice an 11K equirect
  (11520 × 5760), and far above any camera Spherecut reads (the X5's 8K
  is 7680 × 3840). One picture at the limit takes 192 MiB at 8-bit 4:2:0
  and 1 GiB at 8 bytes per pixel, which ProRes 4444 with alpha (decoded
  as 12-bit 4:4:4:4 in 16-bit samples) and 16-bit RGBA PNG take.
  FFmpeg's default `max_pixels` is `INT_MAX`, but its size check
  (`av_image_check_size2`) already refuses a picture whose rows, at 8
  bytes per pixel plus padding, would come to 2^31 bytes: about 2^28
  pixels (16384 × 16000 passes, 16384 × 16300 does not, checked with
  6.1). So `MAX_PIXELS` halves the largest picture a file can make a
  decoder allocate.
- A stream of larger pictures is named by its size, as Spherecut's limit
  and not damage: `probe` warns "stream #0 (sample entry `avc1`) is
  16384x8208, more than the 2^27 pixels (16384x8192) per picture that
  Spherecut decodes" and exits 0, and `VideoDecoder::open` refuses it
  with `MediaError::PictureTooLarge`, before opening a decoder.
  FFmpeg's own account was misleading: its H.264 decoder fails to open
  with "Invalid argument", which `probe` took for a damaged `avcC`, and
  `avformat_find_stream_info` leaves an MJPEG stream at 0x0, which
  `probe` took for a missing frame size; FFmpeg's "Picture size ...
  exceeds specified max pixel count" was only in its log. So
  `open_input` puts back the size the container gave (MP4's sample
  entry, Matroska's `PixelWidth` and `PixelHeight`) when FFmpeg left an
  over-limit stream at 0x0. The check (`TooLarge::of`) pads the width to
  a multiple of 64 first, because FFmpeg checks the limit when it
  allocates a picture, with its rows padded to its SIMD alignment (64
  pixels with AVX-512): its H.264 and MJPEG decoders refuse 16380 × 8194,
  fewer than 2^27 pixels, as 16384 × 8194, so the warning then says
  "which FFmpeg may pad to 16384x8194". That may refuse a picture within
  63 columns of the limit that a build with less padding, or libdav1d,
  which allocates its own pictures, would decode. 16384 × 8192 itself
  decodes (checked with H.264 on 6.1).
- `clippy.toml` forbids `std::net`'s `TcpStream`, `TcpListener` and
  `UdpSocket`, ffmpeg-next's input opens (`format::open`, `open_with`
  and the `format::input` functions), its decoder opens
  (`Decoder::open`, `open_as`, `video` and the like) and its output
  opens (`format::output`, `output_with`, `output_as`, `output_as_with`),
  which take the path as a URL too, so `-o http://...` would write to the
  network. So new code can't open a socket or bypass `open_input`, the
  limit or `Mp4Writer`'s own file handle by accident. The synth crate's
  tests read files they wrote themselves with ffmpeg-next directly, and
  one remuxes into a file of its own, each under an `#[allow]` that says
  so.

Why: the agent integration review (D23) found that `deny.toml` can't see
what FFmpeg does, since FFmpeg is a system C library. M0's probe followed
an HLS playlist to a segment outside its folder, and a playlist with
`http://` entries would make FFmpeg use the network. Each layer covers
what another can't. Limiting the demuxers is the main one, because not
every nested open goes through `io_open`: the concat demuxer opens its
files through a format context of its own, which gets the whitelists but
FFmpeg's default `io_open`. `io_open` stops what the allowed demuxers, or
a later FFmpeg, would open beside the file, and the protocol whitelist
keeps even FFmpeg's own `io_open` to files. With FFmpeg 6.1, M0's probe
already refused the `http://` segment, because HLS limits its segments to
`file,crypto,data`, and the ffconcat file ("Unsafe file name"); the tests
don't rely on either.

Why `file:` and not our own AVIOContext: FFmpeg's file protocol already
seeks, reports the size, retries `EINTR` and honours the interrupt
callback that `VideoDecoder::open_cancellable` uses. ffmpeg-next's custom
IO can't take an interrupt callback (`StreamIo::set_interrupt` is
private to the crate), so our own read, seek and buffer would be more
unsafe code with no more confinement, since `io_open` refuses nested
opens either way. Why identify first: so that the refusal names the
container before its demuxer runs, and the open can't end up with another
demuxer than the one checked. It costs a second open and a read of the
file's start.

Tier 1 tests:

- `ffi` unit tests: `opened_files_are_confined` reads the whitelists,
  `io_open` and `enable_drefs` back from a real context;
  `other_containers_are_refused_before_their_demuxer_runs`;
  `nested_opens_are_refused_and_named`, which allows `hls` so that
  `io_open` alone has to stop a segment outside the playlist's folder and
  an `http://` one; `file_urls_hold_the_path_as_it_is`;
  `decoders_are_limited_to_max_pixels` (a 512 × 128 frame opens at 65536
  and fails at 65535),
  `the_decoders_that_find_stream_info_opens_are_limited`,
  `a_stream_over_the_limit_keeps_its_size` (H.264 and MJPEG under a limit
  of 1000 pixels) and `too_large_pictures_are_named_by_size` (the bounds,
  the padding and the wording).
- `probe` unit test `a_stream_over_max_pixels_is_a_warning_that_names_the_limit`.
- `crates/media/tests/confinement.rs`: `VideoDecoder::open` and
  `probe::probe` refuse a playlist by name, and a URL is a missing path.
- `crates/cli/tests/confinement.rs`: `spherecut probe` refuses, by name
  and with exit 1, an HLS playlist whose segment is outside its folder,
  one with an `http://` segment, an ffconcat file and a WAV file;
  `http://127.0.0.1:1/x.mp4` is "No such file or directory"; files named
  `concat:x.mp4`, `concat:x.mp4|x.mp4`, `subfile,,start,0,end,0,,:x.mp4`,
  `file:x.mp4`, `a:b.mp4` and `http://127.0.0.1:1/x.mp4` are read as
  themselves, not as the `x.mp4` beside them; and under `strace -f` probe
  of all of them opens no `AF_INET` or `AF_INET6` socket and none of the
  files they list. The trace must show the inputs being opened, so a
  trace that saw nothing fails.
- `crates/cli/tests/doctor.rs`: `doctor_opens_no_network_socket`, the
  same strace check, which must see doctor load lavapipe.
- `crates/cli/tests/probe.rs`: a Matroska file with only subtitles still
  fails as having no video or audio stream, and an LRC file is refused
  by name. `a_picture_over_max_pixels_is_a_warning_that_names_the_limit`
  probes MJPEG files from `spherecut_synth::media::write_mjpeg_declaring`,
  64 × 64 frames whose sample entry and frame headers say 16400 × 8200,
  and 16380 × 8194; decoders stop at the header, so the test stays small.
  The branch's first version called the first damaged ("has no frame
  size").
- `crates/synth/tests/decode_errors.rs`:
  `a_picture_over_max_pixels_is_refused_by_size`, the same file through
  `VideoDecoder::open`.

M0's binary under the same strace opened `inside/../outside/private.ts`
and, for `concat:x.mp4`, `x.mp4`, so these tests fail on M0. They pass
on Ubuntu's 6.1.1 and on D2's 9.0.2 build, where every media, CLI and
synth test passes but the six that need FFmpeg's Vulkan device, which
that build lacks (`crates/cli/tests/doctor.rs`, its strace test
included, and `hw_refusal.rs`). The minimal 8.1 build here has too few
demuxers and encoders to run them; for 8.1, Pat's version, the
`io_open` contract was checked in its source.

strace is a Tier 1 prerequisite: `cargo xtask check` stops before
anything else when `strace -V` fails, printing `sudo dnf install strace`
(Fedora), `sudo pacman -S strace` (Arch) or `sudo apt install strace`
(Ubuntu), and the strace tests fail rather than skip without it.
`cargo xtask hw-test`'s prerequisite check lists it too, with the dnf or
pacman command. The tests trace with `--seccomp-bpf`, so only the traced
calls stop the process.

What the confinement changed elsewhere: `probe` refuses other containers
instead of reporting them with `structure: none` (D15). `none` stays,
for MP4 boxes that can't be read, a Matroska header that can't be read
or a Segment with no size, and a container added later without a
structure check. `hw_refusal`'s AV1 test opened the raw `av1-main.obu`,
which needs the `obu` demuxer. Instead of widening the list for a test,
it opens `fixtures/av1-main.mp4`, the same frame remuxed (command in the
fixtures README); doctor still feeds the raw bitstreams to the decoder
and demuxes nothing. The tests use the `hls`, `concat`, `wav` and `lrc`
demuxers only to show that they are refused. `spherecut frame` doesn't
exist yet; its strace test comes with it.

Widening the list: add FFmpeg's demuxer name to `ffi::DEMUXERS` and the
container to `ffi::CONTAINERS`, with a DECISIONS entry that says which
files the demuxer opens besides its input and whether they go through
`io_open` (concat's don't), what `probe` checks of its structure, and
the tests: a file of it probes and decodes, and goes through the strace
test. The likely first are WAV, FLAC or MP3 music beds in M6.

Limits: there is no `codec_whitelist`; any decoder FFmpeg has for a
stream in these containers may run, each under `MAX_PIXELS`. `max_pixels`
bounds one picture, not how many a decoder keeps (references, frame
threads). `TooLarge` checks the size FFmpeg gives the stream, which is
the container's unless a parser reads it from the bitstream, as H.264's
does. So an MJPEG frame header over the limit in a stream whose sample
entry says less still fails while decoding, with FFmpeg's error. A
nested open refused after the file is open, while packets are read, is
only logged; the read that needed it fails as FFmpeg decides. Symlinks
in the path are followed, as by any program; the kernel-enforced roots
for agents are D23's.

Revisit if: Spherecut needs a container outside MOV/MP4 and Matroska; an
FFmpeg release renames these demuxers (the names hold from 6.1 to 9.0) or
opens a format context's input other than through `s->io_open(s,
&s->pb, ...)`; or a camera records pictures larger than 2^27 pixels.

## D25. Zero-copy decode-to-render is proved in M1, decided before M3

*2026-10-24, before M1's code. Pat approved it from the stack review
(the "Spherecut stack review" doc of 2026-10-24). Updates D7 and PLAN's
M9.*

The review kept the stack (Rust, FFmpeg through ffmpeg-next with VA-API
as the main hardware path, wgpu on Vulkan, egui), and found that the
copy of each decoded frame to RAM is already the bottleneck: on huginn,
VA-API with the copy decodes synthetic 5760×2880 at 121 fps against 330
fps in software, and X5 8K30 had one run at 1.17× real time. Leaving
zero-copy to M9 "if profiling shows it matters" would leave M3's raw 8K
dual fisheye on that thin margin.

So M1 adds a Tier 2 test that maps VA-API NV12 and P010 frames from an X5
sample to DRM PRIME (`av_hwframe_map`), imports each plane into wgpu as
its own texture (R8 and RG8, or R16 and RG16), renders one frame, and
compares it with the copy path's output, on huginn and odin, plus an 8K30
X5 playback benchmark done both ways. The decision whether zero-copy
becomes the default is made before M3, from those numbers. The copy path
stays as the fallback, and the overlay and doctor say which path is in
use.

Risk: wgpu 30 imports a DMA-BUF only through wgpu-hal, one plane per
texture, and nobody has yet shown RADV importing AMD's tiled and DCC
VA-API planes that way. If per-plane import fails, the fallback design is
to create the Vulkan device ourselves, give it to both FFmpeg (as its
Vulkan hwcontext device) and wgpu, map VA-API frames to Vulkan inside
FFmpeg, and wrap the resulting images for wgpu. So M1's decoder and frame
types must leave room for a frame that lives on the GPU.

Revisit if: wgpu gains multi-planar external memory import, or Vulkan
Video HEVC decode works on both AMD machines.

*Updated 2026-10-24 by odin's first report* (PR #3, `docs/perf.md`): half
of that revisit condition is met. With RPM Fusion's
`mesa-vulkan-drivers-freeworld`, Vulkan Video decodes H.264 and HEVC bit
for bit on odin's discrete RX 7600 and is the only path faster than real
time on every X5 mode (1.61 to 2.23 ×), while VA-API with its copy falls
to 0.81 × at 8K30; huginn's Vulkan failure was Fedora's own Mesa build
lacking the codecs (D38), not the GPU. So the fallback design above is
no longer only a fallback: slice 5b measures zero-copy both ways, VA-API
frames imported as DMA-BUFs and FFmpeg's Vulkan Video decoding on wgpu's
own Vulkan device, and the default path (and its kind, VA-API or Vulkan)
is chosen from those numbers on odin and huginn before M3 (m1-plan
amendment 9).

*Updated 2026-10-30 by slice 5b's path A, as built* (`docs/design/m1-5b.md`).
VA-API frames map to DRM PRIME (`HwOutput::DrmPrime`), each layer one
plane of one object (NV12 as `R8` and `GR88`, P010 as `R16` and
`GR1616`), and `render::dmabuf` imports each layer with wgpu-hal's
`texture_from_dmabuf_fd`. A frame that won't export so (a modifier with
a second memory plane, such as DCC metadata, or a crop on the left or
top) is downloaded instead, and `VideoFrame::transfer` says why.
wgpu-hal doesn't check that the driver can take an image, and an
image it can't is undefined behaviour, so `Importer` asks first,
through ash: the format's modifiers with one memory plane, sampling and
copies, then an image format query with the DMA-BUF handle type for
importability and the largest size. Each frame is imported afresh,
since wgpu tracks a wrapped texture's layout itself and a cached import
would see no barrier when the decoder writes the surface again; the
upload thread keeps the frame until the GPU work that read it is done,
and the frame cache holds at most `SURFACE_CAP` (16) frames on surfaces
a stream, eight under the decoder's extra 24. Ownership isn't moved
from `VK_QUEUE_FAMILY_FOREIGN_EXT`, which wgpu has no hook for; RADV and
ANV read imported DMA-BUFs without it, and the Tier 2 comparison
catches a driver that doesn't. The copy stays the default: zero copy is
`SPHERECUT_ZERO_COPY=1` in the viewer and `frame --zero-copy`, the
overlay counts frames imported, copied and refused, doctor says whether
each Vulkan device can import, and a refused import is copied, said once,
and asked for no more on that player. The Tier 2 tests and
`bench_x5_8k30_zero_copy_vs_copy`, whose rows the choice before M3
takes, run on odin next; odin's sixth run (PR #8) has VA-API's download
at 31 to 122 ms a frame at p50 (5.7K 8-bit to 8K), the cost this
removes. Path B follows on a branch of its own.

*Measured on odin, 2026-10-30* (PR #9, at f3e860d, `docs/perf.md`).
Path A's Tier 2 tests pass: every frame of HEVC Main, Main 10 and an X5
lens came through its import byte for byte, and no import was refused.
Imported, VA-API decodes the X5's 1920² and 2880² modes about twice as
fast as copied (265 and 136 fps a lens unthrottled), level with Vulkan
Video copied, and its 8K modes three times and half again as fast (76
and 59), where Vulkan Video copied makes 63 and 78. The choice before
M3 waits for path B's rows and huginn's.

*Updated 2026-10-30 by slice 5b's path B, as built* (`docs/design/m1-5b.md`).
FFmpeg's Vulkan Video decodes on wgpu's own device:
`Gpu::headless_shared` and `for_window_shared` open it through wgpu-hal's
`open_with_callback` with the video extensions, synchronization2, and
two compute queues and a decode queue that wgpu never uses, and
`HwDevice::vulkan_shared` gives FFmpeg the raw handles and those queues
(through `qf[]` from 7.1, the old per-kind fields on 6.1). Wrapping the
decoded image for wgpu is ruled out: it sits in decode layouts wgpu
can't name, and on RADV it is a reference the decoder goes on reading.
So `render::vkshare::Copier` copies it, on a queue of Spherecut's own
and under FFmpeg's frame lock, into a device-local buffer, waiting for
and signalling the frame's timeline semaphore as FFmpeg's own work does;
the upload thread waits for the copy, lets the frame go, stages a wait
on the copy for wgpu's next submit, and fills the slot from the buffer
as the copy path does from its staging. That is one GPU copy of the
picture per frame, and none through the CPU. Zero copy on in the viewer
opens the device shared and takes Vulkan Video's frames this way; a
refusal is copied, said once and asked for no more, as path A's is;
`frame --zero-copy --decoder vulkan` renders one frame through it. On
lavapipe, which has no decode queue, a test-only device with one queue
for all checks FFmpeg's frames into the slots byte for byte; the Tier 2
tests (`shared_vulkan_matches_copy_path`,
`shared_vulkan_frame_matches_the_copy`) and the bench's Vulkan Video
zero-copy rows run on odin next.

*Measured on odin, 2026-10-30* (PR #10, at a13ee07, `docs/perf.md`).
Path B's Tier 2 tests pass, every frame byte for byte through the copy
out of FFmpeg's image, none refused, and path A's again. Path B is the
fastest unthrottled path in every X5 mode (277, 143, 78 and 81 fps a
lens), 3 to 9 % ahead of Vulkan Video copied and of VA-API imported,
with no download and a quarter to two thirds of Vulkan Video copied's
upload CPU time. On odin, then, the order is path B, VA-API imported
and Vulkan Video copied about level, and VA-API copied far behind. The
choice before M3 waits for huginn, whose Fedora Mesa has no Vulkan
Video H.264 or HEVC until it gets RPM Fusion's
`mesa-vulkan-drivers-freeworld` (Pat's call), so that only path A can
lift its VA-API copy there for now.

## D26. egui stays, checked against a timeline prototype at the end of M2

*2026-10-24, before M1's code. Pat approved it from the stack review.
Updates D7.*

egui is the stack's weakest part (hand-drawn timeline, weaker text
editing and IME than Qt, `egui_tiles` without floating windows), but it
is the only toolkit where the viewer shares wgpu's device with no
interop code. Every toolkit leaves the NLE timeline to be drawn by hand,
so the question is whether egui stays smooth with a real timeline. At
the end of M2, a prototype with about 500 clips on 6 tracks, drag with
snapping and ripple trim, has to hold display refresh on huginn and odin
with fractional scaling, with UI time per frame in the overlay. If it
doesn't, the problem is written up and Pat decides before any switch.
The prepared alternative is Qt 6 / QML through cxx-qt, sharing a
`VkImage` between wgpu and Qt's RHI as Gyroflow does.

To keep a switch cheap, the render core and the project model hold no
UI types: a switch would replace only `spherecut-app`.

Revisit if: the M2 check fails, or `egui_dock` or egui's text editing
changes enough to matter.

## D27. The CLI JSON contract

*2026-10-24, M1. Updates D6, D18 and D23. The criterion is in PLAN.md
M1; `docs/cli-json.md` is the contract itself, field by field.*

What, for `spherecut probe` and `spherecut doctor` now, and for
`spherecut frame` when it lands (M1 slice 4):

- With `--json`, stdout carries exactly one document per run, on success
  and on failure: one line of compact JSON and a newline. stderr carries
  only the log and the `error:` lines for people. The document is an
  envelope, `{"schema", "ok", "result"?, "error"?, "warnings"}`, and `ok`
  is true exactly when there is no `error`. It is built in
  `spherecut_cli::contract`, the CLI crate's new library half, so that
  A1's `spherecut call` and `spherecut mcp` can reuse it. That is where
  it lives for M1 only: the `spherecut` binary is to host `mcp` and
  `call` on spherecut-agent's types, so spherecut-cli will depend on
  spherecut-agent, and spherecut-agent can't depend back on it for
  `Envelope`, `ErrorBody` or the codes (Cargo refuses the cycle, and the
  library half brings clap, anyhow and tracing-subscriber with it). A1
  moves `contract` into spherecut-agent, or into a small crate below
  both that needs only serde, serde_json, core and media, as the module
  does today, and spherecut-cli re-exports it. The JSON doesn't change.
- `schema` is one string that names the document and its version:
  `spherecut.probe/1`, `spherecut.doctor/1`, and `spherecut/1` for a
  usage error that names no subcommand.
- An error is `{"code", "message", "path"?, "ffmpeg_log"?}`. A code is
  snake_case and stable; the message is for people. The codes come from
  three places: `MediaError::code()` (every variant has one, listed in
  `MediaError::CODES`), probe's (`probe::codes`) and the CLI's
  (`contract::codes`: `usage`, `files_failed`, `doctor_errors`,
  `internal`). `contract::all_codes()` is their union. A refused nested
  open is now its own variant, `MediaError::NestedOpenRefused`, with the
  same message as before, so that it has its own code.
- probe's `result` is always `{"files": [...]}`, for one file as for
  many, each `{"path", "ok", "report"?, "error"?}`. A damaged file that
  FFmpeg can open has both its report and its error. A failed file has
  its own code (`not_found`, `io_error`, `not_a_file`, `not_media`,
  `unsupported_container`, `nested_open_refused`, `truncated`, `damaged`,
  `no_video_or_audio`, or `open_failed` when FFmpeg can't open it for a
  reason probe can't name), and the run fails with `files_failed`, whose
  message names the first. The report's `damage` and `warnings` are
  problems, `{"code", "message"}`, where they were strings: `truncated`
  or `damaged`; `too_large`, `no_decoder`, `cannot_decode`,
  `unexpected_first_box` or `track_without_stream`.
- doctor's `result` is its report, unchanged, whether or not it found
  errors. An issue of severity error fails the run with `doctor_errors`.
- A `Rational` is the string `"n/d"`, reduced, or `"n"` for an integer. A
  `Time` is `{"exact": "<rational>", "seconds": <float>}`, where `exact`
  is the time and `seconds` is for display. The new `core::FrameTime`
  is a frame's time in clip time (0 at the stream's first presented
  frame, M1 plan section 3.3) and adds `frame`, the frame's index from
  that frame, only when it is known: a constant-rate stream, or a frame
  reached by counting, as the plan's `frame --json` result has it. A
  reader must allow it to be absent. All three read back from these
  forms, ignoring `seconds`, and a `FrameTime` reads `"frame": null` as
  absent, as serde reads any other `Option`. probe's container
  `duration` and `start_time` and its streams' `duration` are `Time`s,
  where they were `[num, den]` arrays of exact seconds; time bases and
  frame rates stay rationals, and `start_pts` stays an integer in ticks.
  probe's times are on the container's timeline, where `start_time` may
  be far from 0, not in clip time; `docs/cli-json.md` says which is
  which.
- A panic under `--json`, on any thread, is still the run's one
  document: a panic hook writes an `internal` error that says where it
  panicked, with no `result` (as for a usage error), and the run exits
  1. Only a signal leaves stdout empty: a crash, such as a fault inside
  FFmpeg, or a kill such as Ctrl+C, which probe and doctor don't catch.
  `docs/cli-json.md` tells readers to treat it as a failure, and to
  report a crash.
- Exit statuses, in `contract::Exit`: 0 the run did what was asked; 1 an
  input or a check failed, doctor's errors included; 2 a command line
  that doesn't parse, as a document with clap's message under `--json`
  and as clap's text otherwise; 3 a codec or device the command itself
  needs is missing, and 4 interrupted, both reserved for `frame` and the
  GUI. probe and doctor never exit 3: a missing decoder is a warning in
  probe, as in M0, and a finding in doctor (D18). `spherecut probe` with
  no file is now a usage error, exit 2.
- The versioning rule: within a schema version, fields are only added,
  never removed, renamed or given another type, and a code keeps its
  name and meaning. New codes, and new values of fields that list
  choices (a stream's `kind`, a camera's `layout`), may appear, so a
  reader ignores fields it doesn't know and handles an unknown code by
  the exit status. Any other change is a new version, such as
  `spherecut.probe/2`.

`crates/cli/tests/contract.rs` holds the codes as a literal list, so a
rename fails a test, and checks that `docs/cli-json.md` has a row for
each code and each exit status. A test in media matches every
`MediaError` variant without a wildcard, so a new variant needs a code.

Why: agents and scripts (D23, A1) need one document to parse, with
stable codes on failure as much as on success. M0 printed one object for
one file and an array for several, gave an unopenable file a different
shape, and printed nothing on stdout for a usage error. One shape for
one file or many saves every caller a special case. Times are exact
because a frame's identity has to survive JSON: 1001/30000 s has no
exact float, so a reader of float seconds has to know the rate and
round to find the frame. A rational as text, not a `[n, d]` pair, reads
the same in JSON, ron, the text output and FFmpeg's own notation, and
can't be mistaken for two numbers. Codes on probe's problems let a script
tell a cut-off copy, worth copying again, from a damaged file without
reading English.

Considered: a `schema` name with a separate integer `version`, as the M1
plan had it; one string is one comparison, and a reader can't check one
half and forget the other. JSON lines, one document per file: a run
would then not be one parse, and a usage error would have no place.
Error codes on doctor's issues: their `what` names the codec or device,
and nothing yet needs to branch on them; they can be added as a field.

Cost: `seconds` repeats `exact`, and a reader that compares floats can
still go wrong, which `docs/cli-json.md` warns about. The envelope's
`warnings` is always empty for probe and doctor in version 1, since
their findings belong to their results; it is there for `frame` and A1.
`mp4.warnings` (strings, each also a `damage` problem with a code) keeps
M0's shape. `mp4.top_level` keeps its `[type, size]` pairs but lists
only the first 64 boxes, with `top_level_omitted` counting the rest,
since a fragmented recording has thousands; the text output still
lists them all. A spherical `stereo` is always a name, `unknown` for a
value the specification doesn't define, with the `st3d` byte in
`stereo_value`, where M0 wrote an unknown one as `{"unknown": n}`: one
type per field is what typed readers and A1's schemas need. A video
stream's `color` and `field_order` are FFmpeg's names, and what a file
doesn't say is FFmpeg's `unknown` (`unspecified` for a chroma location),
as in the text output, not null; null is left for a value this FFmpeg
has no name for. Mapping `unknown` to null later would change a field's
values, which is a new version, so it is settled now.

*Updated in M1 slice 4*, for `spherecut frame`, which joins the
contract as `spherecut.frame/1` (`docs/cli-json.md` has every field):

- **The result** is `contract::FrameResult`, there only when the PNG was
  written: a failed run has no `result`, and keeps the warnings it gave
  before it failed. It has `output`; `frame` (`pts` in the stream's
  ticks, and `time`, a `FrameTime` in clip time); `requested`, a
  `FrameTime` too, with `frame` when a frame number was asked for;
  `exactness`; `size`; `depth`; `view` (the angles and projections as
  given); `filter` (the one rendered with); `renderer` (`gpu` or
  `reference`); `decode` (`path`, and `fallback` with `from`, `code` and
  `reason` when a hardware decode went on in software); `gpu` (wgpu's
  `name`, `driver`, `driver_info` and `device_type`, null for
  `--reference`); `color` (the resolved `ColorInfo`, with the file's
  codes and the `assumed` flags, D30); and `source` (`stream`, `codec`,
  `size`, `pix_fmt`). `depth`, `renderer`, `source.stream` and
  `requested.frame` are beyond the plan's list, which said
  `requested` is `{exact, seconds}`: a run asked for frame 17 says so
  in the shape every other frame time has.
- **`exactness` is `exact`**, its only value in version 1: a seek that
  comes back otherwise fails the run (past the end, damage before the
  target), or is a bug (`internal`). Amendment 8 kept `truncated` as one
  of its values; frame never writes it, since it writes no frame that
  isn't the one asked for. The field stays for A1's tools, which may
  return a keyframe for a fast scrub.
- **Codes.** `contract::codes` adds `output_exists`, `not_equirect`,
  `out_of_range` and `cancelled`, and `contract::codes::warnings` frame's
  warnings: `stereo_left_eye`, `stereo_unknown`, `crop_applied`,
  `pose_not_applied` and `anisotropy_unavailable`. A Matroska or WebM
  file is `not_equirect` with its own message: Spherecut doesn't read
  Matroska's spherical metadata (Spherical Video V2's `Projection`
  element) yet, so it can't tell, and `--equirect` renders it.
  `RenderError::CODES`
  (`no_gpu`, `adapter_not_found`, `gpu_failed`, `too_large`,
  `invalid_input`, `internal`, `out_of_gpu_memory`) joins
  `all_codes()`. So does `no_display`, the GUI's, with slice 7: the GUI
  prints no document, and exits 3 with the message.
  Damage frame may not reach is a warning with probe's code: the damage
  probe finds (`truncated` or `damaged`) before the seek, and what the
  decoder meets after the frame (its cause's code).
- **Exit statuses**, from the code by `contract::frame_exit`: `usage` 2;
  `no_gpu`, `adapter_not_found`, `no_decoder`, `hw_unavailable`,
  `hw_unsupported` and `hw_failed` 3; `cancelled` 4; anything else 1.
  The `hw_*` codes reach the run's error only when `--decoder vaapi` or
  `--decoder vulkan` named the hardware (D39): `auto` falls back and
  says why in `decode.fallback`. The plan's table had `hw_unavailable`
  and `hw_unsupported` at 3; `hw_failed` is there too, since a device
  that fails mid-decode is the device's failure, not the file's (D30).
  What clap can't check is a `usage` error with exit 2 in frame's
  schema too: a `--fov` past the projection's limit, `--reference` with
  `--filter aniso`, a `--size` past the GPU's texture limit and a
  `--stream` that isn't a video stream.
- **Amendment 8's mapping**: a seek that damage stops short of its
  target (`Exactness::Truncated`) fails with the damage's cause,
  `DamageCause::code()`: a cut is `truncated`, a read error `io_error`,
  a cancelled read `cancelled` and anything else `damaged`, naming the
  last good frame's time. A cut inside the first frame's own data is
  `decode_failed` ("the video stream has no frames"), since there is no
  last good frame; probe's `truncated` comes first as a warning.
  *Slice 4's review* added: `--frame N` on a variable-rate stream,
  which counts frames from the first, fails the same way when damage
  stops the count (it was `decode_failed`, with no time), naming the
  damage event's last good frame. On a constant-rate stream `--frame N`
  is the frame shown at N's time, and frame checks that it is frame N:
  when it is another, frames are missing there (the decoder lost them,
  or the index skips them), and the run fails `damaged` ("has no frame
  N: at its constant rate frame M is shown in its place"), or
  `out_of_range` when M is the last frame. A `--time` or `--frame` so
  far past the end that the stream's time base can't hold it is
  `out_of_range`, with the clip's range, like any other past the end
  (it was `decode_failed`). A Matroska file shorter than its Segment is
  now a cut the decoder sees (D13), so a frame past it is `truncated`
  rather than the last frame rendered as the one asked for.
- **The order of the checks** is the plan's: the output before any work,
  so a missing input with an existing output is `output_exists`; then
  probe and the layout, the GPU, the decoder, the seek. Whatever fails
  first is the error.
- **SIGINT.** `cli::interrupt`, the binary's one `unsafe` module (D28),
  installs a handler with `sigaction(2)` once, in frame only, after the
  output check and before the temporary file is made: `SA_RESETHAND`, so the default action is back as soon as
  it runs and a second Ctrl+C kills the process (130, no document), and
  `SA_RESTART`, so the system calls it interrupts on other threads are
  restarted rather than failing FFmpeg's reads with `EINTR`, which would
  look like damage. The handler stores `true` in an atomic. The work
  runs on a worker thread; the main thread polls the flag every 10 ms,
  and on the first signal calls `StopSource::shutdown()`, which stops a
  seek between frames and raises FFmpeg's interrupt flag inside a read.
  The other steps (probe, the GPU's set-up, the render, the PNG's
  encode) take no stop token: the worker checks between steps, so there
  the first Ctrl+C takes effect when the step ends, seconds later at a
  large `--size`. Whatever error the worker then returns, the run's is
  `cancelled`, with exit 4 and its document, and the temporary file is
  dropped. When frame starts with SIGINT ignored, as a non-interactive
  shell starts a background job (`cmd &`), it installs nothing and the
  signal stays ignored, as Unix has it for such jobs; it asks for the
  current action first (slice 4's review found the handler replacing
  `SIG_IGN`). probe and doctor make no `rt_sigaction` call for SIGINT
  at all, so Ctrl+C ends them at once, as before.
- Without `--json`, frame prints one line on stdout (`wrote out.png:
  1920×1080 rectilinear 100° at 0.4 s (frame 12, pts 12012), decoded in
  software (VA-API: …), rendered on …`), its warnings go to stderr as log
  lines, and an error as `error:` and FFmpeg's lines.

Tested in `crates/cli/tests/contract.rs`: frame's envelope on success,
on a missing file and on a usage error; exits 0 to 3 with one document
each (no Vulkan driver at all through `VK_DRIVER_FILES` and
`VK_ICD_FILENAMES` naming a file that isn't there, and `--decoder vaapi
--render-node /nonexistent`); Ctrl+C
300 ms into a seek on `long-gop`, which must end with exit 4,
`cancelled`, no output and no temporary file, before the seek would
have ended; a frame started with SIGINT ignored, which a SIGINT then
doesn't stop (`/proc`'s `SigIgn` keeps it, `SigCgt` doesn't get it);
and strace's record of who installs a SIGINT handler (frame: a query,
then the handler). `crates/cli/tests/frame.rs` runs every other error row, among
them `adapter_not_found`, `no_decoder` (a sample entry FFmpeg doesn't
know) and `--decoder vulkan` on lavapipe, which is `hw_unavailable`
since the row shows the Vulkan loader lavapipe's ICD alone, so every
Vulkan device FFmpeg lists runs on the CPU, on a machine with a GPU
too (D37).
`hw_unsupported`, `hw_failed` and `anisotropy_unavailable` can't be
reached here: there is no decoding device to refuse a stream or fail,
and lavapipe has anisotropic filtering. It has the feature only: on
Mesa 25.2.8 (LLVM 20.1.2) its `--filter aniso` renders byte for byte
what `iso` does, on views that squeeze the source (rectilinear 160°,
fisheye 360°, rectilinear 90° at pitch 80°), so `"filter": "aniso"`
there changes nothing, and no test in any tier yet tells aniso from
iso (PROGRESS, Next: render's Tier 2 at H1, or the GUI's slice).

Why the worker thread rather than a flag the work polls itself: the
seek can sit inside one FFmpeg read for a long time, and the main
thread has to be free to raise FFmpeg's interrupt flag there. Why
`SA_RESETHAND`: a run that doesn't stop on the first Ctrl+C (a driver
call that doesn't return) must still die on the second, as the user
expects.

Revisit if: a caller needs codes on doctor's issues or one document per
file of a long run; or A1's schemas want another time or error shape, in
which case that is a version 2 of these schemas, never a silent change.

## D28. Crate layout, rust-version 1.95 and where unsafe may appear

*2026-10-24, M1 slice 3b. The M1 plan's D26 (section 2); updates D1,
whose 1.95 half landed with slice 3a.*

What:

- **Crates**, as slice 3 leaves them: `core` (exact time, the frame and
  colour vocabulary, stop tokens), `sphere` (projection maths, the view
  convention, pixel mapping), `media` (FFmpeg: confinement, probe,
  doctor, decode and seek, D24 and D29), `render` (wgpu: planes, upload,
  mips, projection, encode, stills and the CPU reference, D31), `synth`
  (generated media and the backends that stand in for FFmpeg), `testkit`
  (Tier 2 gates, benchmark recording, the image metrics, FNV-64), `cli`
  (the `spherecut` binary) and `xtask`. `playback` (workers, the frame
  cache and GPU slots), `audio` (cpal and the clock) and `app` (eframe)
  arrive in slices 5, 6 and 7, with the dependency directions of plan
  section 2.1.
- **Directions.** render depends on core and sphere only: never on
  media, and it holds no UI type (D26). Frames reach it through
  `core::video::VideoPlanes`, which media's decoded frames and synth's
  in-memory ones both implement, so the goldens (D32) render without
  FFmpeg decoding anything. playback will never depend on egui. synth
  depends on media, so tests that need both live in `crates/synth/tests/`
  and media's dev-dependencies stay free of synth; render's tests take
  synth's frames, so their binaries link FFmpeg, though nothing in them
  opens a file with it. sphere needs nothing from core yet, though the
  plan drew that edge.
- **rust-version 1.95** for the whole workspace (D1).
- **unsafe.** `#![forbid(unsafe_code)]` at the root of core, sphere,
  render, synth, testkit, xtask and, from slice 5, playback; audio and
  app get it when they land. media has `#![deny(unsafe_code)]` with `#[allow(unsafe_code)]`
  on `mod ffi` and `mod hwprobe`, the two FFI modules D11 allows and the
  only files with `unsafe` in them. cli's binary has
  `#![deny(unsafe_code)]` with `#[allow(unsafe_code)]` on `mod
  interrupt` alone, which slice 4 added: three blocks that zero a
  `sigaction`, empty its mask and install frame's SIGINT handler, whose
  body only stores to an atomic (D27). Its library half (`contract`)
  has `forbid`. Every block keeps a SAFETY comment, which
  the workspace's `clippy::undocumented_unsafe_blocks = "deny"` enforces.
  render stays `forbid` until slice 5b, whose device module opens the
  Vulkan device through wgpu-hal and imports DMA-BUFs, both `unsafe`
  (D31, plan amendments 2 and 9): render then becomes `deny` at its root
  with that one module allowed, as media is.

Where this differs from the plan:

- Through slice 3a only render, sphere and testkit had `forbid`. Slice 3b
  adds it to core, synth, xtask and cli's library, and `deny` with its
  allowances to media and cli's binary, without changing any code: `unsafe` appears only in
  media's `ffi.rs` and `hwprobe.rs`.
- The plan has M9's DMA-BUF import as the first GPU-side `unsafe`, in an
  FFI module of its own. Amendments 2 and 9 moved Vulkan Video and the
  import into slice 5b, so render's exception comes then, in the one
  module that opens the device.
- sphere has no core dependency (above).

Why: M2's export, `spherecut render` and M5 drive the engine with no
window, and the goldens and M3's stitcher feed render without FFmpeg.
`forbid` makes the rule mechanical: an `allow` further in can't lift
it. `deny` at a root with named allowances keeps every exception in one
visible line. bytemuck's derives compile under `forbid` (D22), so render
needs no `unsafe` of its own before 5b. egui 0.37 needs 1.95.

Revisit if: any crate but media, cli and, from slice 5b, render needs
`unsafe`, which then needs an entry that names the module.

*Updated in M1 slice 5b:* render's root is `#![deny(unsafe_code)]`, with
`#[allow(unsafe_code)]` on `pub mod dmabuf` alone: the DMA-BUF import
(an fd duplicated with `BorrowedFd::borrow_raw`, wgpu-hal's
`texture_from_dmabuf_fd` and `create_texture_from_hal`) and the
driver's answers through ash. Path A needs no device of its own, so
the device module stays `unsafe`-free; path B's device through
`open_with_callback` will be the next exception, named here when it
comes. media's `hwprobe` gains one block, the device extension query
behind doctor's `dmabuf_import`, under its existing allowance.

*Updated by slice 5b's path B:* render's second exception is
`#[allow(unsafe_code)]` on `pub mod vkshare`: the shared device through
wgpu-hal's `open_with_callback` and `create_device_from_hal`, the raw
handles given to FFmpeg, and the copier's own Vulkan objects and
submissions through ash (its queue, command buffers, timeline semaphore
and buffers handed to wgpu with `create_buffer_from_hal`). Its one
test-only entry, `single_queue_gpu`, which lets FFmpeg and the copier
share wgpu's queue on lavapipe, is an `unsafe fn` whose contract says
so. media's FFmpeg side, `ffi::vulkan` (the `AVVulkanDeviceContext` made
from the handles, and the frame lock around the copy), sits under
`ffi`'s allowance.

## D29. MediaBackend and frame-exact seek

*2026-10-24, M1 slice 2a. Updates D13. The M1 plan's D27 (section 3.3,
`docs/design/m1-plan.md`); hardware decoding and fallback join it in
slice 2b.*

What:

- Everything above media opens media through `backend::MediaBackend`
  and decodes through `VideoDecode`. `FfmpegBackend` opens a confined
  (D24) software decoder; synth's `SyntheticBackend` and `GatedBackend`
  stand in for it in tests. Hardware decoding through the trait (slice
  2b) and audio (slice 6) return `MediaError::NotYet`, code
  `not_implemented`, until they land.
- Clip time 0 is the first presented frame: `origin_pts` is its pts,
  and clip time is (pts − `origin_pts`) × `time_base`, whatever an edit
  list or B-frame delay does (ARCHITECTURE.md, "Time").
- `seek(at, Exact)` returns the frame with the greatest pts at or before
  T = `origin_pts` + ⌊at / tb⌋, later frames winning ties. It says how
  that frame relates to `at`: `Exact`; `BeforeFirst` (at < 0, the first
  frame); `AfterLast` (at or past the end of the last frame's interval,
  the last frame); `Truncated { last_good_pts }` (the file is damaged
  before T: the last frame before the damage, with a `Damaged` event; a
  seek to exactly that frame's pts is `Exact`, with the event). A frame
  that comes out after reading has failed, as the decoder drains, may
  follow frames the damage lost (a cut inside B-frame 13 loses 13 and 15
  but not 14 and 16), so a seek whose first frame past T is one is
  damaged too, and the "already showing it" shortcut doesn't trust it.
- The algorithm is seekprobe's, as the plan gives it: a BACKWARD seek to
  T, and when the first frame out is after T (and isn't the first frame
  of the stream), a seek again to just before the keyframe it landed on,
  up to `MAX_ATTEMPTS` (8) times, then from the start. A seek past the
  cut of an MP4 cut short, whose `moov` lists samples the cut took, lands
  where nothing can be read; it seeks again to the last keyframe whose
  data the file holds, by its dts plus the least lag (the MOV demuxer
  shifts a seek's target back by it), and decodes forward to the cut,
  rather than from the start. The landing is the first packet read after
  the seek, by its dts, or its pts when it has none, as Matroska's
  packets mostly don't: the timestamp the demuxer's index and
  `av_seek_frame` compare. A target in the GOP being
  decoded decodes forward with no seek: the keyframe the index names for
  T has been read (for H.264, the last before T that may start an exact
  decode, below). The index lists keyframes by dts, so T is shifted back
  by the least lag of pts behind dts over the keyframes read, as the MOV
  demuxer shifts a seek's target. The index is used only when it lists
  something after the last packet read (a Matroska demuxer's lists only
  the keyframes read until it has read Cues); otherwise, and with no
  index, T within a second decodes forward.
- `seek(at, Keyframe)`, for scrubbing: one BACKWARD seek and the
  keyframe it lands on, whatever its pts, flagged `KeyframeOnly` (its
  leading pictures are dropped, below). It is after T when T is one of
  that keyframe's leading pictures; otherwise an exact seek to T after it
  decodes forward, unless FFmpeg doesn't vouch for an H.264 keyframe
  (below). Past the cut of an MP4 cut short it is the last
  keyframe the file holds, with a `Damaged` event. Step back is an exact
  seek to pts − 1.
- Frame N of a constant-rate stream is at (N + ½) frame durations
  (`seek::frame_address`, `seek_frame`); of a variable one, counted from
  the start. An MP4 track's rate is constant when every sample but the
  last lasts the same in `stts` (in a time base of one over the track's
  timescale) and its `ctts` offsets show the frames as evenly: every
  sample on one slot of the decode-time grid, shifted alike, no two in a
  slot and none left out, as reordered B-frames are (tracks of more than
  2^26 samples count as variable), and its edit list, if it has one,
  shows the media once: one edit that isn't empty, at rate 1. FFmpeg
  drops frames where one edit's piece meets the next, so frame N of a
  spliced track isn't where its rate puts it. FFmpeg sets a Matroska
  track's average and real rates both from its default duration, which
  says nothing of its blocks, so its rate is constant when the two agree
  within a millionth of a standard rate (FFmpeg reads 59.94 as
  19001/317), the first 64 packets' pts, read at open and sorted, lie
  one frame apart on that rate's grid to the millisecond (bar the last
  16, which B-frames may still fill, unless the stream ended), and every
  Cues entry lies on it. The seek back to the start after that read is
  the demuxer's first seek, which is when it reads Cues written after the
  clusters, as FFmpeg writes them.
- The stream ends where its `duration` says (MP4), else Matroska's
  per-track `DURATION` tag, else the file's duration: `info().duration`
  and `AfterLast` both use it. When the container says nothing, or ends
  the stream at or before its last frame (a raw MOV whose last sample has
  no duration), the last frame's pts plus its duration, else the average
  frame duration, takes its place, and `info().duration` says so once
  that frame has been decoded.
- FFmpeg hands out the frames it flags corrupt
  (`AV_CODEC_FLAG_OUTPUT_CORRUPT`), and Spherecut drops them: decoded
  from references a seek skipped, they are not frames of the stream as a
  decode from the start shows it. After a seek away from the start, and
  for H.264 at open and from the start too, it also drops, by pts, the
  frames shown before the keyframe decoding began at that come out
  before it: its leading pictures (open-GOP B-frames, RASL and RADL
  pictures; at the start FFmpeg drops HEVC's RASL pictures itself, and
  its RADL pictures there are frames of the stream). FFmpeg 6.1's
  H.264 decoder flags those it decoded without their references, except
  as it drains at the end of the stream; RADL pictures decode whole,
  but are not the keyframe. A target among them is reached by the seek
  again to before that keyframe.
- `next_frame` returns an error only when the stream can't go on (a
  read failed), and again on every call until a seek; damage it decodes
  past is a `Damaged` event, as in a seek.
- Seeks stop through `core::cancel`'s generation tokens, checked before
  each packet is read and each frame is taken, which leaves the demuxer
  in a known state. The next call goes on just after the frame handed
  out last: it seeks (to just before that frame's pts when the stream has
  tied pts, from the start where every seek goes there) and decodes up to
  it without making it again, finding the frame after it by its place
  from the start where that is known, else by pts and its rank among
  the frames that tie with it. A rank counts from where decoding began,
  so the seek goes before their pts only when decoding had begun before
  the first of them, and else to their pts, landing where it was
  counted from; a frame seen to be the last of its ties needs no rank.
  When that can't tell the frame apart for sure, the frame after it
  starts a run of its own. `next_frame` hands a frame out only once the
  frame after it is known (for its duration), and keeps it for the next
  call when stopped first. FFmpeg's interrupt flag (`VideoRequest::interrupt`)
  is for shutdown only, since it ends the stream for good (D13).
- A seek clears a read failure and the end of the stream, so a seek back
  into the good part of a truncated file decodes again. It does not
  clear a device's refusal. This updates D13, where a read error was
  sticky.
- Of frames that tie, the later is shown, and the decode keeps the
  frame with the greatest pts at or before T, not the last one out.
- Timestamps that go backwards in decode output mark the stream not
  exactly seekable (`DecoderEvent::NotExactlySeekable`), and its timing
  variable, since frame N is then the Nth decoded. Exact seeks then
  decode the whole stream from the start for the frame shown at T and
  again up to it, before any shortcut, and the seek that first sees the
  step does itself again so; they say `AfterLast` and `Truncated` as
  other seeks do. A stream whose frames have no timestamps gets index ×
  frame duration and always decodes from the start.
- A frame carries its `run` and its `seq`, its place in decode output in
  the run, so that frames of one run with consecutive `seq` are adjacent.
  The cache links runs by them. A run starts at each demuxer seek (a seek
  that decodes forward goes on with the run before it, and so does the
  next call after a stopped seek), and at the first frame handed out
  after packets went in while catching up skipped frames, since a frame
  the decoder skips leaves no trace. Every frame decoded takes a `seq`,
  handed out or not, in `FfmpegVideo` and `SyntheticBackend` alike.
- `catch_up(Some(t))` skips non-reference frames (`AVDISCARD_NONREF`)
  until a packet with a pts at or after t is sent, and returns no frame
  before t. Packets go in in decode order ahead of the frames coming out,
  so stopping at the first frame out at or after t would already have
  skipped B-frames just after it. A seek ends it; so does
  `catch_up(None)`, and a later t carries it on. Ended (by a seek, by
  `None` or by an earlier t) after it passed frames over, it leaves the
  decoder to find its place again, so the next frame is the one after
  the last returned and a seek doesn't decode forward over the gap. A
  stream whose frames are counted from the start skips none.

Where this differs from the plan: the landing falls back to pts
(Matroska); `forward_is_cheaper` asks whether the index's keyframe for T,
shifted by the keyframes' lag, is at or before the last packet read (the
plan says the next one to be read, the same GOP test), and uses the
index only when it lists what lies ahead; the end of the stream falls
back to the last frame's interval when the container says nothing or
ends it too early; corrupt frames are output and dropped rather than
left to FFmpeg, and a seek's leading pictures are dropped by pts; a seek
past an MP4 cut starts from the last keyframe the file holds; Matroska's
constant rate is checked on its first 64 blocks and its Cues, not every
block; MP4's needs `ctts` and the edit list as well as `stts`; the trait
method is `decode_path()`, not `path()`, beside the file path; frame
addressing is free functions; and catch-up ends on a packet's pts.

Why: seekprobe measured a naive BACKWARD seek missing 11 of 40 frames on
open-GOP HEVC and 8 of 40 on H.264: the leading pictures after a keyframe
whose dts is at or before T but whose pts is after it are dropped without
a word. The seek oracle (`crates/synth/tests/seek.rs`) checks every clip
of the set against a sequential decode, by pts and by a hash of the
planes, at four times in each frame's interval, in order and shuffled,
and the frames after each; the naive seek misses frames of both open-GOP
clips, and n frame durations misses frames of the Matroska remux. FFmpeg
6.1's H.264 decoder, left to drop corrupt frames itself, also loses the
frames still in its reorder buffer at the end of the stream: after a
seek to the last keyframe of an open-GOP clip its last frame never came
out. The half-frame offset survives Matroska's millisecond timestamps
(at most 0.5 ms off, against half an interval of at least 8 ms at
60 fps). The review of slice 2a found each later rule by a wrong frame
returned as `Exact`: a cut mid-GOP gave frame 12 for 13; a stream whose
pts go backwards gave the last frame out rather than the one shown; a
variable-rate Matroska file with a 30 fps default duration gave 28 of
its 30 frames wrong by index, as did an MP4 whose `ctts` tie two frames;
a seek after a stopped catch-up gave the frame before the target; and
on one thread (as slice 2b's hardware decoders run) catching up called
frames adjacent across a frame the decoder skipped. A seek 9 s into a
300-frame Matroska file decoded 271 frames forward before its Cues were
read. The oracle now runs on tied and backwards clips, in MP4 and
Matroska, and checks each seek's cost (one seek and its re-seeks, at
most two GOPs decoded) and every run link. Its second review found more.
After a seek to keyframe 24 of a 25-frame open-GOP H.264 clip, the
leading B-frame decoded without its references came out unflagged at
the drain and was returned as `Exact` for frame 23, and as
`KeyframeOnly`; a keyframe seek on HEVC with RADL pictures gave a RADL
picture. A seek past the cut of a 3000-frame MP4 cut short decoded 1989
frames from the start (now 9), and a keyframe seek there gave frame 0
without a word. An MP4 spliced from two pieces by its edit list read as
constant-rate. Resuming after a stopped seek gave keyframe 8 again when
it tied with frame 7, and in a backwards stream skipped or repeated a
frame, each linked as adjacent. The oracle now runs on those clips too.

Limits: the timestamp-less path can't be reached through the two
demuxers Spherecut opens and has no test. A not-exactly-seekable stream
decodes twice per exact seek, and is found out only when its timestamps
are seen to go backwards: seeks before then trust them. `forward_is_cheaper`
knows a keyframe's pts only as its dts plus the least lag seen, so a
target among the leading pictures of the next keyframe seeks (and seeks
again) rather than decoding on. A Matroska file whose blocks leave its
default duration's grid after the first 64, between Cues entries, still
reads as constant-rate. The run started after catching up skipped is
conservative (any packet sent while skipping starts one) and no test
reaches it: every path a test can drive has decoded the frame after the
last one before skipping begins. It guards a keyframe seek stopped just
after its frame and a frame out after a decode error. Resuming in a
stream whose timestamps go backwards, after a frame handed out before that
was found out (so without its place from the start), takes the first
frame shown at its pts for it, and the frame after that starts a run.

Revisit if: the cache or the GUI needs something `run` and `seq` don't
give; a decoder outputs frames out of pts order in streams that aren't
broken; or FFmpeg's H.264 drain is fixed and the corrupt frames can be
left to it.

*Updated 2026-10-24, against FFmpeg 8.1.3 (D2).* FFmpeg 7.1 and later's
HEVC decoder queues the frames it outputs (all it has left at the end of
the stream, several at once where a keyframe empties its picture
buffer), and `avcodec_flush_buffers` leaves that queue as it is. On one
thread, as hardware decoders run (D30), a frame from before a seek then
came out after it: a keyframe seek into the last GOP of the open-GOP
HEVC clip, after an exact seek there and the frame after it, gave the
clip's last frame, and an exact seek took such a frame for timestamps
going backwards. Before a seek's flush the decoder now hands out, and
the seek drops, whatever it holds ready (at most 64 frames, which don't
count as decoded): with no packet sent, a decoder gives only what it has
made, or what its frame threads are finishing. The oracle's
`on_one_thread_no_frame_from_before_a_seek_comes_out_after_it` opens
open- and closed-GOP HEVC and open-GOP H.264 on one thread and, for every
frame, seeks it, takes the frame after, seeks the keyframe there and a
frame elsewhere; on 8.1.3 it fails without the fix.

From 7.1 the HEVC decoder no longer returns `AVERROR_INVALIDDATA` for a
picture whose slice header or frame start it can't decode: unless
`AV_EF_EXPLODE` is set, it skips the picture with a warning ("Skipping
invalid undecodable NALU"), where 6.1 returned the error from
`avcodec_send_packet` and Spherecut said it as a `Damaged` event. And
from 8.0 it flags the pictures that refer to one it lost
`AV_FRAME_FLAG_CORRUPT`, up to the next IRAP picture, where 6.1 decoded
them from what it had. For the spliced MP4's second keyframe, whose POC
the decoder still holds, the same frames come out either way, none
flagged, so the splice test expects the event only before 7.1. For a
damaged reference picture they don't: Spherecut dropped every frame
flagged corrupt as one decoded from references a seek skipped, so on
8.1.3 (and 9.0.2) the rest of the GOP was lost without a word, and an
exact seek into it returned an earlier frame as `Exact` with no event.
Now only the frames flagged corrupt before the first good frame since
opening or a seek are dropped silently: their references come before
where decoding began. After it, for HEVC (and every codec but H.264,
below), the first of a run of them is damage,
a `Damaged` event like any other the decode goes on past, and it and
the rest of the run are dropped. A seek into the frames lost stays
`Exact` for the frame the decode from the start shows there, with the
event, as damage on 6.1 is. A `Damaged` event raised while decoding
ahead of the frame handed out now names the last frame decoded as
`last_good_pts`, as the event's doc says, rather than the one handed out
before it. `damage_that_loses_a_reference_picture_is_reported_and_seeks_agree`
breaks the slice header of the picture after a GOP's keyframe in
closed-GOP HEVC, with and without B-frames, and in H.264, on frame
threads and on one, and checks the event, that nothing outside the
frames that refer to it is lost or changed, and every exact and keyframe
seek against the decode from the start; on 8.1.3 it fails without the
fix. FFmpeg 7.1.x (libavcodec 61.19 up to 8.0's 62.11) neither returns
the error nor flags the frames, so there the damage is silent: the test
expects no event from HEVC there.

*Updated 2026-10-24, against FFmpeg 8.1.3 and 7.1.3.* That rule took
H.264's corrupt flag for damage too, and it isn't. The H.264 decoder
flags a frame only by where it has seen decoding recover since it began
(an IDR picture, a recovery point SEI, or an I-frame it takes for a
place to start), never for a reference picture it lacks, which it
conceals or fails `avcodec_send_packet` over. From 7.0, in a stream with
B-frames, the frames decoded after such an I-frame and before it comes
out don't count as recovered, and come out flagged though every
reference they need is there; 6.1 hands them out unflagged. In a stream
whose keyframes after the first are I-frames that aren't IDR, with no
recovery point SEI, every seek to a keyframe but the first then raised
a `Damaged` event on 7.x and later, and lost those frames. Now the flag
after the first good frame is damage for every codec but H.264, whose
flagged frames are handed out (`corrupt_is_damage`); before it the flag
still marks, for every codec, the frames decoded from references a seek
skipped. Of FFmpeg's other decoders, VVC's flags as HEVC's does, and
those for AV1 (FFmpeg's own, libdav1d and libaom), VP8, VP9, MPEG-1, 2
and 4, ProRes and MJPEG flag none. The oracle's
`h264_keyframes_without_recovery_points_seek_exactly` strips the SEI
from open-GOP H.264 with 3 and with 2 B-frames, and from one cut at a
keyframe that isn't IDR; FFmpeg flags neither those keyframes nor their
packets as keys, so there the oracle takes the MP4's sync samples
(`stss`) for keyframes. It fails without the fix on 8.1.3 and 7.1.3. The
oracle now also fails a clip on any `Damaged` event, from a decode from
the start or a seek, except the HEVC splice up to 7.0.

*Updated 2026-10-28.* That rule's clips keep 1 reference frame, as
synth's `veryfast` does, which FFmpeg's heuristic needs. Keeping 3, as
x264's default `medium` does, SEI-less open-GOP H.264 lost almost every
frame after a seek on every FFmpeg from 6.1: the decoder fills the gap
in `frame_num` a seek (or a cut's start) leaves with 3 stand-in
references, more than the heuristic allows, so it starts at no keyframe
but an IDR picture and flags every frame up to the next. Spherecut
dropped them all but those FFmpeg drains, unflagged, at the end of the
stream: a cut at the second keyframe of a 40-frame clip opened on its
second last frame and played 2 of its 32 frames, nearly every keyframe
seek gave the clip's second last frame, and exact seeks were right but
decoded from the start, all without an event. For H.264 a flagged
I-frame decoded from the first packet read since opening or a seek
(the file's first, or the keyframe the index lists and the seek landed
on) is now the first good frame (`starts_here`): its slices refer to
nothing. The frames shown before it stay dropped, by their flag and by
pts. Since FFmpeg's parser flags such a keyframe's packet only by its
recovery point SEI, the landing's pts is now taken whatever the
packet's flags: else the leading B-frames FFmpeg drains unflagged after
a seek to a clip's last keyframe were handed out. The landing also
counts now for the keyframes' least lag, which in such a cut no packet
flagged a keyframe gave: with 1 B-frame, an exact seek after a keyframe
seek that landed before its target sought again.
`h264_keyframes_without_recovery_points_seek_exactly` adds SEI-less
clips keeping 3 references, of 30 frames and of 25 (which end on a
keyframe and its leading pictures), a cut of one, and a cut with 1
B-frame; each fails without its fix on 6.1 and 8.1.3.

*Updated 2026-10-28, after its review.* That rule handed out, as
`Exact`, frames that weren't the decode from the start's. Decoding from
a flagged I-frame is right for a cut that starts there and for a
keyframe seek's frame, but nothing in the file says that the frames
after it don't refer to frames before it, and where they do, they refer
to FFmpeg's grey stand-ins. x264 codes a frame it is made to code as an
I-frame before `min-keyint` (at a flash or a scene cut) with no recovery
point, and keeping 3 references it lets the frames after it refer across
it; some muxers list every I-frame in `stss`. On such clips 28 of the 40
exact seeks to a clip's frames, each on a fresh decoder, gave a frame
unlike the decode from the start, with no event, on 6.1 and 8.1.3, and
17 of a batch of 20 on one decoder did. No margin of frames decoded
before the target would do: the stand-ins pass on through every chain
of references. So an exact seek now ends only on frames decoded where
FFmpeg vouches for the decode (`exact_run`): from the stream's first
packet, or, for H.264, from a packet its parser flags a keyframe (an
IDR picture, a recovery point SEI, or an I-frame of a stream whose SPS
keeps one reference frame) when FFmpeg then says decoding has recovered
by the first good frame: it flags that frame a keyframe, or doesn't flag
it corrupt before the stream drains (it flags nothing as it drains).
The decoder's own heuristic was not to count: it takes an I-frame for a
place to start wherever the stream allows few references, and with 2
the frames after one still referred across it. (It still counted, and
the stream's first packet needs a decoder as opening leaves it, which a
flush isn't: the next update makes both so.) Every other codec's
decoders flag a frame that refers to a picture decoding didn't reach,
so for them any landing counts, as before. Where a seek landed elsewhere, the exact seek seeks
again to before the landing, as for a leading picture, a keyframe at a
time and then from the start (`MAX_ATTEMPTS`), and no exact seek
decodes forward from the frames decoded since: `forward_is_cheaper` and
the "already showing it" shortcut need `exact_run`, and for H.264 the
keyframe the index names for T is the last before it that wasn't read
and seen unflagged. A keyframe seek still gives such an I-frame,
`KeyframeOnly`, and playing on from it, or from the start of a cut
there, hands out every frame decoded from it, as d4bfe81 made them. The
rule was to be exact as far as the file says, and wasn't (next update).
Its cost falls on H.264 whose sync samples FFmpeg doesn't flag: there
an exact seek decodes from the start of the stream, so its cost grows
with the target's place in the stream, not with the GOP. Measured on 6.1 and
8.1.3 alike, by frames decoded and seeks (`SeekCost`, summed over an
exact seek to every frame of a clip, each on a fresh decoder, and over
a batch of 20 random exact seeks on one decoder; the machine's load left
the times meaningless): 40-frame clips with I-frames listed at 10 and
22 went from 336 frames and 33 seeks to 900 and 81 (at most 42 frames
for one seek, from 18), their batch from 168 frames to 304; the 30-frame
SEI-less clips keeping 3 and 1 references from 191 to 524 (13 to 33 at
most), their batch from 136 to 244. H.264 with recovery points or
closed GOPs and HEVC decode the frames they did (open-GOP H.264 191 and
136, closed 150 and 128, noisy 1920x960 H.264 and HEVC 961 and 296). Of
`bench_seek_latency`'s clips, the synthetic one and the X5's are HEVC,
unchanged by construction. The X5's LRV proxies (seven of them in
`spherecut-samples`) are H.264 High, 1664x832, of P-frames keeping 1
reference, with an IDR picture every 30 frames, and their `stss` and
FFmpeg's packet flags name those alone (64 of the 1904 packets of
`LRV_20261022_170245_01_009.lrv`), so their exact seeks decode what
they did: on a 95-frame copy of that one, an exact seek to each frame
on a fresh decoder decoded 1474 frames in all and at most 31 for one,
none wrong, before this rule and after it, and after the next update
too. `h264_i_frames_listed_as_sync_samples_seek_exactly`
writes such clips with 3 B-frames and 1, 3 references and 2, and one
that flashes at the first forced I-frame, checks that FFmpeg's parser
flags no forced frame and that a cut from each decodes otherwise than
the clip, and runs the oracle on copies that list them in `stss`;
without the fix it fails on 6.1 and 8.1.3, on a wrong frame after an
exact seek. The oracle now expects no exact seek to decode forward from
a keyframe FFmpeg doesn't flag, and bounds such a seek's cost by a
decode from the start; `h264_keyframes_without_recovery_points_seek_exactly`
still checks that a keyframe seek gives the sync sample and that an
exact seek in a cut decodes forward from its start.

This entry said the frames shown before such an I-frame stay dropped
by pts, which held only after a seek away from the start. FFmpeg drains
a stream's last frames unflagged, so a Matroska cut at the last keyframe
of open-GOP H.264, which keeps that keyframe's leading B-frames (shown
before it, from pts 0), opened on one of them, decoded without its
references, and so did decoding from the start again after a stopped
seek. For H.264 the pts rule now holds at open and on every seek to the
start too. HEVC keeps it after seeks away from the start only: at the
start FFmpeg drops RASL pictures itself, and RADL pictures are frames
of the stream. The decoder's read position, which `forward_is_cheaper`
compares with the keyframe a seek would land on, is now the greatest
timestamp read since the landing, not the last: Matroska's pts go back
at B-frames, and in that cut the last packet read was shown before its
keyframe, so an exact seek after a keyframe seek sought again.
`open_gop_h264_seeks_exactly_and_a_naive_seek_does_not` makes the cut
with the SEI and without; each of the three changes fails it without
its fix on 6.1.

*Updated 2026-10-28, after the review of that rule.* Two things it
leaned on didn't hold, and where they failed it returned wrong frames as
`Exact`, with no event.

FFmpeg's heuristic still counted. Its parser flags every packet with a
recovery point SEI a keyframe, whatever count the SEI gives, so muxers
list it. Where such an I-frame says decoding is right only some frames
on (at x264's next keyframe) and the stream keeps 2 references, or 3
with 1 B-frame on frame threads (the SEI doesn't reach every thread's
context), FFmpeg's heuristic takes the I-frame for a place to start and
flags nothing after it. The first good frame was then unflagged, the
landing counted, and the frames up to the recovery point, decoded from
stand-ins, came back `Exact`: of 60 exact seeks on fresh decoders, on
frame threads, the review counted 30 wrong with 1 B-frame and 2
references, 18 with 3 B-frames and 15 with 1 B-frame and 3 references,
alike on 6.1, 7.1.3 and 8.1.3. Now, for H.264, only a first
good frame that FFmpeg flags a keyframe starts an exact run after a
seek: an IDR picture, or a picture whose recovery point counts no
frames, where the file says nothing before it is needed. The parser's
one-reference case goes too: FFmpeg flags no such frame a keyframe, and
nothing Spherecut reads tells its packet from that of an I-frame whose
SEI counts frames. A landing that doesn't start an exact run is noted,
and the keyframe the index names for T passes over it as over a packet
seen unflagged. `h264_recovery_points_that_count_frames_seek_exactly`
gives the I-frames forced at 10 and 22 a recovery point counting 4
frames, with 1 and 3 B-frames, 2 and 3 references and a flash, lists
them, and runs the oracle in MP4 and Matroska; it fails without the fix
on 6.1 and 8.1.3.

And a flush doesn't leave FFmpeg's H.264 decoder as opening does: its
frame pool keeps the pictures it decoded, where a new pool's buffers are
zeroed (`av_buffer_allocz`). A stream that starts at an I-frame that
isn't IDR and has no recovery point (a cut there), with no B-frames and
3 references, refers to stand-ins for the frames the cut left out, and
FFmpeg's heuristic fires on the first of them before it would colour
them grey, so they keep what their buffers held: zeros after opening,
and earlier pictures after a flush, which differ with what was decoded
before. So decoding from the start again gave other pictures than
opening did, and every exact seek that went there returned them: the
review counted 34 to 40 of a batch of 40 on one decoder, in MP4 and
Matroska, on 6.1 and 8.1.3, and plain FFmpeg gave 34 of 38 frames unlike
the decode from opening after a flush, on 6.1, 7.1.3 and 8.1.3, and
none after opening the decoder again. Now a decode from the stream's
first packet, once packets have been sent, runs on a decoder opened
afresh as it was at open, unless the first frame decoded since opening
was a keyframe (an IDR picture, or a recovery point counting no
frames), after which nothing from before is needed. The flush still
serves every other seek, and a stream that starts on a keyframe, as
every encoder's does. `a_cut_at_an_i_frame_decodes_from_its_start_as_it_opens`
cuts P-only x264, keeping 3 references, at an I-frame its scene-cut
detection made (synth's clips can now cut away to another scene, with
that detection on), checks that the cut decodes otherwise than the clip
and alike twice, and runs the oracle on it with its I-frames listed, in
MP4 and Matroska; it fails without the fix on 6.1 and 8.1.3.

With both, the rule is exact as far as the file says: what an exact
seek returns comes from a decode that began at the start of the stream,
on a decoder as opening leaves it, or at a picture FFmpeg flags a
keyframe because the file says nothing before it is needed. It costs
more where seeks leaned on the heuristic. Measured on 6.1 with the
review's probe (an exact seek to each frame on a fresh decoder, by
frames decoded summed and most for one seek; frame threads): the
60-frame clips with I-frames whose SEI counts 8 frames went from 30, 18
and 15 wrong to none, and from 478 frames (at most 18) to 766 (27), 573
to 744 and 622 to 766; the P-only cut from 24 wrong to none, at the
same 843 frames; x264's intra refresh with keyint 8 and no B-frames
(below) from 8 of 40 wrong to none and from 297 frames (at most 17) to
931 (44), and with 2 B-frames, which was right, from 399 to 931; and
x264 without B-frames keeping one reference, which was right, remuxed
with `-c copy` so its 4 I-frames that aren't IDR are listed, from 485
frames (at most 25) to 1339 (52), in MP4 and Matroska alike: more than
the 1175 (47) it takes with only its IDR listed, as x264 wrote it, as
each first landing on such an I-frame is decoded, noted and sought
before (the parser's one-reference case, below). An
SEI-less clip and the LRV decode what they did, and so, by
construction, do closed GOPs, open GOPs with recovery points, and HEVC.

Limits (from 8.1.3): on 7.1.x HEVC damage to a slice header or frame
start is silent, as above. Outside H.264, a frame flagged corrupt after
the first good one is taken for damage: a decoder that flagged a frame
of an undamaged stream so would raise a `Damaged` event for it. The
oracle cuts the SEI-less clips with 1 and 3 B-frames, not the one with
2: some of its keyframes are shown 2 frames after their decode time and
others 3, and the least lag puts the latter a frame early, so in a cut
of it, with the SEI or without, an exact seek after a keyframe seek
that landed before its target seeks once more than the oracle allows
(`forward_is_cheaper`'s estimate, above); its frames are right. An
exact seek in H.264 whose sync samples FFmpeg doesn't flag keyframes as
frames decodes from the start of the stream, however far in its target
lies, even where the frames after them refer to nothing before them
(an I-frame of a stream that keeps one reference frame, or a recovery
point that told the truth): nothing Spherecut reads says so. So does
one in H.264 whose random access points are recovery points on P-frames
(x264's intra refresh), each landing decoded to where FFmpeg counts
decoding recovered and then sought before, up to `MAX_ATTEMPTS` times.
There an exact seek returned what FFmpeg took for recovered, which in
x264 clips with keyint 8 was unlike the decode from the start for 17 of
80 exact seeks without B-frames and 4 of 80 with 2, as an earlier
review counted; now none is, at the cost above. The oracle has no
intra-refresh clip, and with 3 B-frames and 3 references a keyframe
seek there gives the first frame FFmpeg took for recovered, not the
sync sample. Stepping back or seeking exactly through a long such file
is slow; keyframe seeks aren't. After a keyframe seek to such a
keyframe, playing on hands out the frames decoded from it, which may
refer to stand-ins where the decode from the start doesn't. On a
device, whose surfaces FFmpeg doesn't zero (nor colour grey: it leaves
stand-ins as they are under hardware acceleration), a stream that
starts at an I-frame that isn't IDR, with no B-frames, need not decode
alike each time it is opened, and nothing here checks it. An H.264
stream whose first IDR picture has frames shown before it (a picture
order count that isn't its least) loses them at open; no encoder here
writes one.
`VideoDecoder::open`, which tests and the Tier 2 checks use, doesn't
ask for corrupt frames, so from 8.0 FFmpeg refuses the HEVC pictures
that refer to a lost one and it loses them without an error (D13).

*Updated 2026-10-28, after the checks of main at 498d6e1.* A review of
the reopen and the landing rule found no wrong frame and three faults,
now fixed; the third's fix also ends two older ways one damage was said
twice.

A stream copied from mid-GOP (`ffmpeg -ss T -c copy`) starts at the
keyframe before T, behind an edit list that hides its frame, and FFmpeg
drops the frames an edit list hides. So the first frame decoded wasn't
that keyframe, FFmpeg flagged it none, and every seek that landed on the
stream's first packet opened the decoder afresh (on a device a new
VA-API context or Vulkan video session), though a flush gives the same
frames: the review measured 20 to 25% more time on those seeks, in
software. Now the first packet's NAL units say whether its picture is
one FFmpeg's decoder flags a keyframe: in H.264 an IDR picture, or a
reference picture with a recovery point SEI that counts no frames; in
HEVC an IRAP picture of the base layer (NAL unit types 16 to 23). That
needs the length-prefixed form an `avcC` or `hvcC` box describes (MP4,
Matroska); in Annex B and other codecs FFmpeg's flag on the first frame
decided before and still does. A first packet the units don't vouch for
leaves the rule as it was, so HEVC that starts at a picture that isn't
IRAP still reopens: the pool's leftovers were measured for H.264 only,
and nothing here shows whether HEVC's decoder needs it, so it is left.
`DecodeStats::reopens` counts reopens.
`a_trimmed_stream_seeks_to_its_start_on_the_decoder_it_has` hides an IDR
picture (H.264 and HEVC, behind an edit list of 3 frames), an I-frame
with a recovery point counting no frames (H.264) and a CRA picture
(HEVC), each from a trim at 2 frames past the second keyframe, runs the
oracle on each, and makes 5 seeks that land on the first packet, with
no reopen; without the fix the first clip reopened 5 times. The cut at
an I-frame that isn't IDR still reopens, once per such seek, which
`a_cut_at_an_i_frame_decodes_from_its_start_as_it_opens` now counts.

A reopen that failed (for want of memory, or on a device of decoders)
dropped the stream's first packet, and the flushed decoder decoded on
from the second: its error read as damage, with a `Damaged` event, and
frames decoded without the start came back. With the failure injected,
an exact seek to frame 2 of a 12-frame clip returned frame 6 as `Exact`
after three `Damaged` events, and on the stand-in for a device, the
fallback tests' seek to frame 5 returned frame 8 as `Exact`, with no
fallback. Now nothing more is decoded. On a device the failure is the
device's (`HwFailed`, D30), and the fallback goes on in software; in
software it is an error that this and every later call returns until a
seek, never damage and never a failed read.
`a_reopen_that_fails_is_an_error_until_a_seek_and_never_damage` (a hook
in the decoder, test builds only),
`a_reopen_that_fails_on_a_device_is_the_devices_failure` and
`a_decoder_the_device_cannot_open_again_goes_on_in_software`
(`Fault::Reopen`) fail without the fix.

Decoding can meet one damaged packet more than once between two seeks,
and it was said each time. From an earlier landing after the landing rule
turned one down: each exact seek to frames 13, 15, 20 and 25 of
`h264_i_frames_listed_as_sync_samples_seek_exactly`'s clips, damaged in
the packet after the first listed I-frame, gave two `Damaged` events,
where a decode from the start gives one, on frame threads and on one.
At the cut of a file cut short of its index, after a keyframe seek past
the cut had said it and play went on to it. And on the way back to the
last frame handed out after a stopped seek, which said playback's
damage again with the next frame. `spherecut frame` warns once per
event. Now damage is said once from one seek that hands out a frame to
the next, however often decoding meets it. Each event's damage is told
by what two decodes of the same packets share: a decoder error by the
last packet sent before it (byte position and decode timestamp) and how
many errors came out since that packet was sent, since FFmpeg hands out
packet p's result as packet p + n - 1 is sent on n frame threads, and as
p is sent on one, wherever decoding began; a run of frames FFmpeg flags
as referring to a lost picture by its first frame's pts; and the failure
reading stopped with by its cause, as it ends the stream until a seek.
A seek that stops or fails goes on with what was said before it and
what it said. A fallback carries what was said to software, and the
seek it does again doesn't say what the device's attempt said (D30).
Dropping a turned-down landing's events instead would have fixed only
the first case; the event said is the first meeting's, whose
`last_good_pts` is `None` where it is the landing's.
`damage_is_said_once_from_one_seek_to_the_next` covers the three, and a
stand-in for a device failing before and after the damage while playing
and in an exact seek; each part fails without the fix, and a seek after
says the damage again. Damage on a packet with neither a byte position
nor a timestamp, and a lost picture's run without a pts, can't be told
apart and are said each time.

Also found, and left: in a closed-GOP clip of 27 frames (keyframes every
8 frames, 3 B-frames) behind an edit list of 3 frames, in H.264 and in
HEVC, an
exact seek to a keyframe right after a keyframe seek there seeks again
(1 seek, 10 frames decoded) where the oracle's cost check expects it to
decode on, so the oracle fails such a clip at frame 13. Its frames are
right, and it is so with these fixes and without them.

*Updated 2026-10-28, after the review of `m1/seek-nits`.* The review
found one more fault, older than these fixes, and it is left. FFmpeg's
H.264 decoder can hand out a picture it never decoded. Where a damaged
slice header still parses as far as the start of its picture, FFmpeg
starts the picture, fails the slice and hands the picture out with
whatever its buffer held, with no flag, no `decode_error_flags` and no
error from `avcodec_send_packet`; only its log says "decode_slice_header
error". The frames that refer to it, up to the next IDR picture, are
decoded from those contents, which depend on what the decoder decoded
before. So a decode from the IDR picture before the damage differs
there from the decode from the start, and an exact seek returns its own
decode's frames as `Exact`. Measured on the review's clip, 40 frames of
x264 without B-frames keeping 3 references, with IDR pictures at 0, 10
and 22, with the first 6 bytes of packet 12's slice header zeroed
(`break_slice_header`): in `ffmpeg -threads 1`'s decode, frame 12 is
frame 8's picture byte for byte, and plain FFmpeg decoding from IDR
picture 10 on the same decoder after a flush gives other frames 12 to
21, on one thread and on frame threads, on 6.1, 7.1.3, 8.1.3 and 9.0.2,
while the decode from the start flags no frame and fails no packet.
Spherecut's exact seek to frame 13 returns a frame unlike the decode
from the start's, and so do the 8 frames after it, as `Exact` and with
no `Damaged` event, on one thread and on frame threads, on a decoder
just opened and after playing 30 frames (6.1). With packet 11 broken
too, which fails before its picture starts, `avcodec_send_packet` fails
on it and the seek says the damage once, but its frames are as wrong:
on one thread, and on frame threads after playing 30 frames (the
review's case). The review's case is the same at 92db604, before these
fixes; HEVC wasn't tried.
`damage_that_loses_a_reference_picture_is_reported_and_seeks_agree`
holds because its broken slice header fails before FFmpeg starts the
picture. Matching the decode from the start would take a decode from
the start of the stream, on a decoder as it was then, for every exact
seek past such damage, and on a device not even that, as its surfaces
aren't zeroed. Saying the damage would take FFmpeg's log, read per
decoder. And what such a seek should return is open: the plan's rule
for damage before T (the last good frame, `Truncated`) and D27's
(damage before the target fails `spherecut frame`), against this
entry's (a seek into frames FFmpeg decoded past damage is `Exact`, and
`spherecut frame` warns, in words that put the damage after the
frame). Decided 2026-10-28: the entry's rule is for damage FFmpeg
says; damage that only its log says is damage all the same, and once
read from the log it follows the plan's rule and D27's (`Truncated`
before T, and `spherecut frame` fails). Reading FFmpeg's log per
decoder (its callback names the codec context) comes after slice 5;
until then this is a limit.

## D30. Pixel formats and colour defaults

*2026-10-24, M1 slice 2a. The M1 plan's D28, sections 3.4 and 3.5;
extended in slice 2b with hardware decoding, below.*

What:

- A pixel format is uploaded as it is when `AVPixFmtDescriptor`, read as
  ints (D11), says it is planar or semi-planar (U before V),
  little-endian, 8 to 16 bits with every colour component at one depth,
  in the low bits or, at 16-bit samples, the high ones (P010, P210):
  YUV, planar RGB in FFmpeg's G, B, R order, or gray (`ffi::pixel_layout`).
  Alpha is left out. That is the plan's list (`nv12`, `nv16`, `p010le`,
  `p210le`, `p016le`, `yuv{420,422,444}p` and `yuvj*` at 8 to 16 bits,
  `gray`, `gray10le`, `gray12le`, `gbrp` to `gbrp12le`) and every other
  format of the same shape (`yuv440p`, 9-bit, `nv24`, `yuv411p`).
  `PixelLayout::msb_aligned` is false at 16 bits, where high and low are
  the same.
- Every other format goes through swscale on the decode worker, to the
  native format of the same kind at 8, 10, 12 or 16 bits, whichever holds
  its depth (`ffi::conversion_target`): `gbrp*` for RGB, palette, Bayer
  and XYZ, `gray*` for gray, and `yuv420p*`, `yuv422p*` or `yuv444p*`
  for YUV by its subsampling (`yuv444p*` for any other). The range stays
  what it is, the frame keeps its colour tags, and the flags are
  bit-exact with accurate rounding, so a repack (UYVY to `yuv422p`, RGB24
  to `gbrp`) is exact. `DecoderEvent::Converted` says so once. A format
  without a descriptor, or one swscale doesn't take, is
  `MediaError::UnsupportedFormat` (`unsupported_pixel_format`).
- `color::resolve` turns a frame's codes into the colour shown. A field
  left unspecified, or coded in a way Spherecut doesn't render, takes a
  default and sets its `assumed` flag:

  | Field | Default |
  |---|---|
  | range | full for `yuvj` formats and RGB, else limited |
  | matrix | BT.709 at width ≥ 1280 or height > 576, else BT.601; identity for RGB |
  | transfer | SDR |
  | primaries | BT.709 (coded primaries other than BT.709 and BT.2020 are kept as `Other`) |
  | siting | centre for MJPEG, else left; not flagged where chroma isn't subsampled (4:4:4, RGB, gray), since none matters there |

  YCgCo, constant-luminance and ICtCp matrices take the size rule;
  transfers other than SDR's, PQ, HLG and linear are shown as SDR.
- synth writes 4:2:0 chroma as the average of each 2×2 block, so its
  clips are tagged centre-sited (`CLIP_COLOR`, which changes probe's JSON
  for them from `left` to `center`), and 4:2:2 chroma from the left
  column, co-sited, which is what H.264 and HEVC mean by 4:2:2. x264 and
  x265 write a chroma location for 4:2:0 only (FFmpeg 6.1.1's
  `trace_headers`): for 4:2:2 and 4:4:4, FFmpeg's H.264 decoder reports
  left, its default, and its HEVC decoder unspecified, which
  `color::resolve` takes as left and flags as assumed for 4:2:2 alone.

Where this differs from the plan: the plan sent every converted format to
`yuv444p` or `yuv444p16le`; and its native list is a list, where the
rule here covers more.

Why: converting 4:2:2 or 4:4:4 to 4:2:0 on the CPU loses chroma and
costs 10 to 30 ms per 5.7K frame, so what the GPU can sample is uploaded
as it is. Converting to the same kind, depth and subsampling resamples
nothing, keeps chroma siting meaningful, and keeps RGB out of a YUV
matrix; it is also less data than 4:4:4. Untagged files are common, and
the size rule is mpv's and libplacebo's (swscale assumes BT.601, and
libavcodec doesn't guess); the flags let a person or an agent see a
guess.

Limits: only the formats a rawvideo MOV can carry (UYVY, RGB24) are
converted end to end in a test; the rest are checked by format. Formats
of more than 16 bits, float and hardware formats are never uploaded as
they are.

Revisit if: a camera's format falls outside the rule and swscale's
conversion costs too much (M3's raw formats), or the renderer wants a
format kept that this converts.

*Extended 2026-10-24 in slice 2b: hardware decoding through the
backend, the render node, fallback and the pooled download (section
3.5). Updates D3 and D14.*

What:

- `DecodeMode::Hardware { kind, device, fallback }` decodes on VA-API or
  Vulkan Video, on `device` or, when it is `None`, on the backend's own.
  `FfmpegBackend` opens one device per kind, the first time a decoder
  asks for it, and keeps it in a `OnceLock` with the `HwError` that kept
  it from opening, so every decoder of the backend shares the device and
  the answer. Both kinds open on the render node below, the Vulkan one
  through `open_vulkan_device` (D38), and a Vulkan device on the CPU
  counts as none. Where no Vulkan device is listed as the one behind the
  node, even a node `--render-node` names, Vulkan takes the first GPU
  listed, as D38 does, while VA-API fails with `NoDevice`; the log line
  that says a device opened names the node for VA-API and the GPU for
  Vulkan, with why it was taken. Nothing here prefers one kind: slice 5
  picks it from doctor's checks (m1-plan amendment 9).
- The render node (`media::hw`, over a sysfs root that tests fake) is
  that of the PCI device at the bus id wgpu gives its adapter
  (`<sys>/bus/pci/devices/<id>/drm/renderD*`); else the first
  `renderD*` in `<sys>/class/drm` whose device has the adapter's vendor
  and device ids, both, since an AMD iGPU and an AMD card differ only by
  device id; else `/dev/dri/renderD128`. `--render-node` and
  `FfmpegBackend::on_render_node` override it. wgpu arrives in slice 3,
  so until then no adapter is known and the lookup gives the default.
  `RenderNode` says which node and why ("/dev/dri/renderD129, the GPU at
  PCI 0000:03:00.0").
- Hardware decoders are single-threaded and keep `extra_hw_frames`
  surfaces (`VideoRequest::extra_hw_frames`, 2) beyond their own. Frames
  stay on the device until they are handed out: a seek holds its best
  candidate and the frame after it as device frames and downloads only
  the frame it returns, so what it decodes on the way is never copied.
- `ffi::download_into_pool` copies a device frame into a buffer of the
  decoder's `DownloadPool`: an `AVBufferPool` of one buffer per frame,
  holding every plane of the frames context's `sw_format` at the frame's
  size with rows 256 bytes apart (`av_image_get_linesize` rounded up),
  each plane starting on a 256-byte boundary, plus FFmpeg's input
  padding. The buffer goes back to the pool when the last `VideoFrame`
  holding it drops. A new format or size (the `LayoutChanged` case)
  rebuilds the pool, and frames from the old one stay valid. FFmpeg
  crops device frames only on the right and bottom (`decode.c`), so the
  left and top crop is applied after the copy. Both paths crop exactly
  where the stream says (H.264's frame cropping, HEVC's conformance
  window), as `ffmpeg` itself has since 7.1: `open_decoder` sets
  `AV_CODEC_FLAG_UNALIGNED` on every decoder, and the download crops
  with `AV_FRAME_CROP_UNALIGNED`. FFmpeg's default, aligned cropping
  keeps up to 63 of the columns cropped on the left by a rule that
  depends on the pixel format: YUV420P and NV12 keep different ones, and
  P010 fails (`AVERROR_BUG`), so a stream cropped on the left would not
  come out the same on both paths. VA-API and Vulkan both take a
  destination with any strides (Vulkan may import its memory), so one
  path serves both, and `VideoDecoder::next_frame` uses it too; doctor's
  one-frame probes keep `ffi::download_frame`.
- A device that turns a stream down is `HwUnsupported`. Any other error
  decoding or downloading a frame on the device is `HwFailed`
  (`hw_failed`), sticky like a refusal and never the file's damage: a
  hardware decoder's error can't be told apart from damage it met, and
  software, after a fallback, says which it was. *From 2026-10-28 so is
  a decoder the device can't open afresh to decode the stream's start
  again (D29); before, the stream's first packet was lost and the error
  taken for damage.*
- `Fallback::Software` (`fallback.rs`):
  - **At open**, a device that won't open (`DeviceUnavailable`, in
    `MediaError::Hardware`'s words), a refusal (`Refused(HwRefusal)`) or
    a failure on the first frame (`Failed { after_pts: None }`) opens the
    stream in software, which queues `DecoderEvent::FellBack { from,
    reason }` first.
  - **Mid-stream**, `HwFailed` or `HwUnsupported` from `seek` or
    `next_frame` reopens the stream in software, once, and the software
    decoder takes over the hardware one's place: the last frame returned
    and where it is in its run, the run and `seq` counters, the stream
    facts learnt so far, and catch-up's target. `next_frame` then
    repositions as after a stopped seek and returns the frame after the
    last one returned; a seek the device failed in is done again. So the
    frames handed out are software's, with no gap and no repeat, and
    runs link only frames that follow on. The reason is `Failed {
    after_pts, error }`, after the last frame returned. Events come in
    order: the hardware decoder's, `FellBack`, software's, and
    `LayoutChanged` when software's layout or size differs (NV12 to
    `yuv420p`). *From 2026-10-28 software also takes over the damage
    said since the last seek, and a seek done again doesn't say again
    what the device's attempt said (D29).*
  - Every frame after carries `DecodePath::FellBack { from, reason }`,
    `stats()` counts both decoders' frames, and a `tracing::warn!` gives
    D14's reason: "decoding in software instead of with VA-API:
    <reason>".
  - **`Fallback::Fail`** returns the error: `Hardware`, `HwUnsupported`
    or `HwFailed`. Every Tier 2 test uses it.
  - The overlay's decoder line and badge, the GUI notice and `frame`'s
    stderr note and JSON warning come with the overlay, the GUI and
    `frame`. *frame's came in slice 4 as its result's
    `decode.fallback` and its text line, not as a warning (D39).*
- Media's `fault-injection` feature, which only synth's and playback's
  dev-dependencies turn on (so only test builds have it), adds
  `VideoRequest::fault`. Its
  `FaultPlan` fails frame k, by the order the decoder makes frames in, as
  decoding or downloading it on the device fails (`HwFailed`, "fault
  injected: ..."), on a real device or on a software stand-in labelled as
  one, so that Tier 1 runs every fallback path (`crates/synth/tests/
  fallback.rs`, with its `FaultBackend`, and the player's in
  `crates/playback/tests/engine.rs`, D34). The Tier 2 tests check their
  own checks the same way: they pass on a stand-in and fail on a fault.
  *From 2026-10-28 `Fault::Reopen` fails instead every opening of the
  decoder afresh for the stream's start (D29).*

Where this differs from the plan:

- `DecodeMode::Hardware`'s `device` is an `Option`, `None` for the
  backend's own device.
- The backend's `OnceLock` holds `Result<Arc<HwDevice>, HwError>`, not a
  `String`; `HwError` is `Clone` and has `NoDevice { node, reason }`.
- `FallbackReason::Failed` has `after_pts: Option<i64>`, the last frame
  returned, where the plan had `at`: software goes on after it, and it is
  `None` when no frame was returned.
- Mid-stream, software takes over the hardware decoder's place rather
  than running `seek_exact` to the pts after the last frame returned. It
  is the same frame, but found without guessing the next pts in a stream
  whose timestamps tie or go backwards, and with the run and catch-up
  state the seek oracle checks carried over.
- A refusal mid-stream (FFmpeg withdrawing the device's format after
  frames came out) is reported as `Failed`, with the refusal's words.
- On lavapipe with FFmpeg 6.1, H.264 and HEVC are refused as `Withdrawn`,
  not `NotOffered`: FFmpeg has Vulkan hwaccels for them, which fail to
  start because lavapipe has no video queue. MPEG-4 Part 2, with no
  Vulkan hwaccel, is `NotOffered`. The Tier 1 test expects whatever
  `VideoDecoder` reports, and checks the MPEG-4 clip is `NotOffered`.
- `FaultBackend` is in the test file, not `fake.rs`: it needs the
  feature, which only synth's dev-dependency turns on.
- The Tier 2 tests' noisy 5.7K clips are 2 s (60 frames), made by the
  tests and cached in Cargo's temp dir, at about 200 Mbps (x265
  `veryfast`, CRF 18) rather than 100. hw-test's 20 s clips came with
  slice 5 (section 12.4; D37's perf clips).
- The pooled download serves Vulkan too, which the plan left open.
- `DecodeStats` has `download_buffers`, the buffers the download pool
  has allocated, which the pool tests check.
- Software frames of a stream cropped on the left are cropped exactly,
  where slice 2a kept FFmpeg's aligned cropping: their planes may start
  off a 32-byte boundary (swscale, converting one, warns once that this
  is slower), and their size is the one the stream's parameters give,
  which probe reports too.

Why: one device per kind keeps the GPU's decode sessions and memory to
what playback needs, and keeping the error means a missing device costs
one try per backend, not one per clip. Downloading only the frame a seek
returns saves a copy per frame decoded on the way, the copy that makes
VA-API slow on odin (amendment 9). A pooled buffer saves the page faults
of a fresh 25 or 50 MB buffer of ours per 5.7K frame (FFmpeg's VA-API
download still makes its own, below), and 256-byte rows let
render's staging belt copy it in one go (`write_texture` copies in one
go only at its own staging pitch, the row rounded up to the device's
`optimalBufferCopyRowPitchAlignment`, which 256-byte rows meet where the
row is a multiple of 256 already or the device asks for 256; slice 3b
found this, D31). Taking over the hardware decoder's
place makes the continuation exact in every kind of stream the seek
oracle covers, and the fault injection checks it at every frame.

Limits:

- The `*_download_pool_reuses_buffers` tests check the pool by what it
  allocates (`DecodeStats::download_buffers`, counted by the pool's own
  allocator): some buffers in the 30 frames of warm-up, none after.
  The decoding thread's minor faults per frame are reported, not
  limited. *Updated 2026-10-24 by odin's run at 67fa1fd (PR #4):* the
  plan's limit of 50 (section 12.2) was a budget for everything that
  thread does, and on odin's RX 7600 VA-API took 116 per 3840x1920 NV12
  frame and 227 per P010 frame, about one per 95 KB copied, with no
  buffer allocated after the warm-up: the driver's faults, about 10 per
  MB, as radeonsi maps each plane through a staging copy to read it. At
  that size the faults can't check the pool either: glibc hands a freed
  block of up to 32 MiB back from its heap, so even a fresh buffer per
  frame faults in nothing (measured with glibc 2.39). The same reuse made
  the tests' check that the fault counter counts see nothing in 8 MiB
  written, after the VA-API test had freed its frames; it writes 64 MiB
  now, which glibc always maps afresh, and a counter that still counts
  nothing leaves the faults out of the report rather than failing the
  pool check.
- FFmpeg's VA-API download (`av_hwframe_transfer_data`, from 6.1 to
  8.1.3) maps the surface for reading without `AV_HWFRAME_MAP_DIRECT`, so
  it creates a VAImage, which Mesa allocates with `malloc`, has the
  driver copy the surface into it, copies that into our buffer, and
  destroys it: a second full copy, and a fresh frame-sized block per
  frame. Above glibc's 32 MiB (5760x2880 P010, 8K NV12) that block is a
  new mapping each frame, about 12 000 faults and 18 ms more per frame in
  a model of it here. The pool tests report a 5760x2880 P010 clip to
  measure it on the GPU. Slice 5 checks mapping the surface directly
  (`AV_HWFRAME_MAP_READ | AV_HWFRAME_MAP_DIRECT`, vaDeriveImage, where
  the driver allows it) or keeping one image per decoder, before its
  playback benchmarks, unless 5b's zero-copy has replaced the download
  by then.
- An `HwFailed` on a damaged packet hides the damage until software
  decodes the packet again. *Until 2026-10-28 a `Damaged` event could
  then be reported twice, when a seek was done again; now damage is said
  once from one seek to the next across the fallback (D29).*
- The fallback reopens the file and decodes from a keyframe, so a
  mid-stream fallback costs one seek.
- *Updated 2026-10-30 by H1 on odin (PR #7, and the timelines of the
  5.7K benchmarks at f87dc2a):* while the frame cache filled, every
  frame downloaded into a buffer the pool had just allocated, and each
  of its 4 KiB pages faulted in on first touch: about 270 000 faults a
  second, 12 000 a 5.7K P010 frame (one a page of its 50 MB), with
  THP set to `madvise` as Fedora sets it. Vulkan Video's 10-bit download
  took 40 to 51 ms (p50 a second) then, against 18 to 21 once the cache
  was full and buffers came back to the pool, and the decoder ran at 20
  to 24 fps for the first 7 s, under real time; 8-bit took 21 ms against
  9 to 11. The pool's buffers are now mappings of their own, 2 MiB
  aligned and advised `MADV_HUGEPAGE`, so a new 50 MB buffer faults in
  25 huge pages. Here (a VM, THP `madvise`) the first copy into a fresh
  5.7K P010 buffer takes 13.9 ms on huge pages against 172.8 ms on 4 KiB
  ones, and 5.0 ms into one reused. The kernel still zeroes each page it
  hands out; where THP is `never` nothing changes. On odin at 1991a98
  the cache sat in 8.1 to 8.6 GiB of huge pages, a new frame's 10-bit
  download took about 14 ms and a reused one's 5 to 6 (18 to 21 before),
  and the cache filled in 2.5 s rather than 7. The process still counts
  12 150 minor faults for each new frame, as before: most likely the
  GPU's import of the buffer walks it a 4 KiB page at a time and counts
  every page it touches, huge or not; Vulkan Video's reused buffers
  count none. With free memory left in single pages, the kernel
  compacted 395 times in the first 4.6 s, and a new frame's download
  took 15 to 18 ms. Once in four runs, a new frame's download took 50
  to 56 ms for 3 s while the cache passed 5 GiB (a reused one's still 5
  to 6), for a reason not known; `/proc/vmstat` wasn't sampled then.

Revisit if: slice 5 needs a device per stream rather than per backend,
slice 5b's zero-copy replaces the download, or new frames' downloads
take 50 ms again while the cache fills (then with `/proc/vmstat`
sampled beside the timeline: a pool that makes its buffers ahead on a
thread of its own, using huge pages only while the kernel has free ones,
is the fix to try).

## D31. The render pipeline

*2026-10-24, M1 slice 3a. The M1 plan's D29 (sections 4.1 to 4.5);
updates D7. Slice 3b adds the goldens (D32) and the Tier 2 tests.*

What:

- **The device.** wgpu 30.0.1 on Vulkan only (`Gpu::instance_descriptor`).
  `AdapterChoice` is `Auto` (discrete, integrated, virtual, then CPU, so a
  box without a GPU renders on lavapipe), `Cpu`, or a name or PCI bus id;
  `SPHERECUT_ADAPTER` names one for the CLI and GUI, and tests pass
  `Cpu` and assert `DeviceType::Cpu`. The device asks for the adapter's
  timestamp features where it has them, wgpu's default limits with 2D
  textures raised to the adapter's own limit up to 16384, performance
  memory hints, and no texture format features. Every device is made in
  one private function, `gpu::open_device`, and `Gpu::from_parts` wraps
  it. For that to hold in the GUI too, slice 7 opens render's device
  itself and hands it to eframe through egui-wgpu 0.37's
  `WgpuSetup::Existing` (`WgpuSetupExisting { instance, adapter, device,
  queue }`): `Gpu::for_window` is `headless`'s path (the instance from
  `instance_descriptor`, `select_adapter`, `open_device`), run before the
  window. On Vulkan the instance needs no display handle; egui-wgpu
  checks that the window's surface can present from the adapter when it
  makes the surface, and fails the window if not, so no check of ours
  comes before it. The plan's
  `WgpuSetup::CreateNew` (section 7) has egui-wgpu make the device with
  its own `request_device`, which never passes through `open_device`, so
  5b's Vulkan Video device wouldn't reach the viewer; the first review
  found the contradiction. Slice 5b replaces `open_device` alone: it opens the Vulkan device
  itself through wgpu-hal 30's `vulkan::Adapter::open_with_callback`, to
  add the video-decode queues and extensions FFmpeg's Vulkan Video wants
  and the DMA-BUF import extensions, then wraps it with
  `Adapter::create_device_from_hal`, and imports frames with
  `vulkan::Device::texture_from_dmabuf_fd` (amendments 2 and 9). Those
  calls are `unsafe`, so 5b's module is render's first exception to
  `forbid(unsafe_code)`.
- **Errors** are `RenderError`, each with its code in the CLI contract:
  `no_gpu`, `adapter_not_found`, `gpu_failed` (a device refused, a
  validation error, a failed readback, a lost device), `too_large`,
  `invalid_input` (an empty size, a plane that doesn't match its layout,
  a PNG that won't read), `internal` (a frame uploaded into a slot of
  another shape) and `out_of_gpu_memory`.
- **A lost device** is an error, never a panic. `Gpu::from_parts` (which
  `headless` uses too) sets the device's lost callback, which marks the
  `Gpu` lost, and its uncaptured-error handler, which wgpu's default
  makes a panic: after a loss the handler ignores the errors every call
  then raises, and before one it logs an error no scope caught and keeps
  the first for `Gpu::check`. Every entry point that touches the device
  calls `check` first and returns `gpu_failed` (`GpuFrame::new`,
  `Uploader::upload`, `Projector::record`, `LinearTarget::new`,
  `StillRenderer::render` and `render_still`, readback), and
  `Uploader::poll` stops reading timers. 3a's first version installed
  neither, so an upload after a loss panicked in wgpu's handler
  (`Device::destroy` in a Tier 1 test now shows each entry point
  returning `gpu_failed`). `Projector::record` returns a `Result` for it,
  where the plan's returned nothing. Slice 5's engine then sets
  `gpu_error`, sends `GpuFailed` and stops its uploads (D34); what the
  viewer does next (make the device again, re-upload) is slice 7's.
- **Canonical planes**, as plan section 4.2 has them: 8-bit planes are
  written straight into `R8Unorm` or `Rg8Unorm`; 9- to 16-bit planes go
  into an `R16Uint` or `Rg16Uint` scratch, and an ingest pass writes
  `R16Float` or `Rg16Float` holding min(code >> shift, 2^b − 1) / (2^b −
  1) rounded to the nearest half, with a shift of 16 − b for P010 and 0
  for LSB-aligned formats (the clamp keeps stray high bits from reading
  past 1). Every format is one wgpu guarantees without optional
  features.
- **f16 stores are rounded in the shaders.** lavapipe converts a
  fragment's f32 into an f16 attachment by truncating, which Vulkan
  allows: measured at 3a's first review, 510 of the 1024 10-bit codes
  came out of the ingest pass a half below their nearest one (up to half
  a code), 1023 of the 4096 12-bit codes (up to two codes), and 124 of
  256 grey levels in the linear target. Every pass that writes f16
  therefore returns a value that is a half already, the nearest one with
  ties to even, so that the store is exact on any driver: the ingest
  pass works it out from the code in integers (no GPU division or log2
  can move a code near a tie, and 12-bit codes come within an f32 ulp of
  one); the mips of deep planes and the projection round their f32 with
  `round_f16` in `shaders/common.wgsl` (bit arithmetic, a scale by 2^24
  for subnormals, and a signed infinity from 65520 up, where the nearest
  half is infinite). The 8-bit mips and the encode pass round to codes
  themselves: the mip pass works each mean of codes out exactly, in
  integers, and returns its nearest code, ties to even, over 255; the
  encode pass returns its target's nearest code, and into an f16 target
  the nearest half (both below, after odin's third run).
- **Mips**: one area-weighted shader for all four formats, a render pass
  per level, reading the level above with `textureLoad` and weighting
  each texel by its overlap in integer arithmetic, so odd sizes come out
  exact. At 8 bits the mean of the codes is an integer division too,
  rounded to the nearest code with ties to even (amended 2026-10-28,
  below). Levels per plane are clamp(1 + ⌊log2(max / 64)⌋, 1, 8).
- **Slots**: `GpuFrame` holds a frame's planes with their mips, its
  `Source` uniform and its bind group, for a `SlotShape` (plane formats
  and sizes; the depth may differ, since the ingest normalises it). Level
  0 of each plane is an ordinary texture that a buffer copy, a texture
  copy or a render pass can fill, so a frame already on the GPU (slice
  5b) fills a slot without `VideoPlanes`: it writes level 0 of
  `GpuFrame::plane_texture`, records the mips with
  `Uploader::record_mips` and sets the uniform with
  `GpuFrame::set_source`, as an upload does (a Tier 1 test fills a slot
  that way and gets the same levels and projection). Slots are made
  inside an `OutOfMemory` error scope.
- **Upload**: `Uploader::upload` writes the planes, then records ingest
  and mips with GPU timestamps around them where the device has
  `TIMESTAMP_QUERY_INSIDE_ENCODERS` (so `UploadStats::gpu_ms` is the GPU
  time of both, on either path; 3a's first span began after the ingest
  passes and timed only the mips, which a Tier 1 test now catches),
  writes the `Source` uniform and submits once, never waiting. `UploadPath::WriteTexture` (the default)
  passes the frame's rows to `write_texture` at their own stride;
  `UploadPath::StagingBelt` copies them into a reused belt chunk at a
  256-byte pitch and records `copy_buffer_to_texture`.
- **The projection** (`shaders/project.wgsl`): rectilinear, equidistant
  fisheye and stereographic, and a blend of two θ(ρ) curves for a morph.
  The LOD is the shader's own: central differences of the direction
  half a pixel either side, through `uv_gradient`, λ = log2 of the
  longer side of the footprint in texels, clamped to the chain.
  `Filter::Isotropic` samples with `textureSampleLevel` at λ (goldens,
  the CPU check, `spherecut frame`); `Filter::Anisotropic` with
  `textureSampleGrad` and a 16× sampler (the GUI). `DebugMode` replaces
  that with implicit derivatives, level 0, or a heat map of λ. Colour is
  one 3×4 matrix from `ColorInfo`, folding the range and Kr/Kb by H.273,
  applied to samples read as code / (2^b − 1), then clamped, through the
  sRGB EOTF (or not, for linear sources) into an `Rgba16Float` target;
  the encode pass applies the OETF into `Rgba8Unorm`, one texel per
  pixel with no filtering, rounded to 8-bit codes in the shader. A
  16-bit still reads the linear target back and applies the OETF on the
  CPU.
- **Samplers**: U repeats and V clamps; U clamps too for an sv3d crop.
  `Placement` gives the cover, the crop and the eye.
- **Stereo**: of a stereo frame only the shown eye, the left, is
  uploaded (its rows from the frame's top-left corner, at the frame's
  stride), into a slot of that eye's size (`Placement::shown`). U then
  wraps at the eye's own ±180°, V clamps at its own poles, its mips never
  mix in the other eye, and the upload is halved. 3a's first slots held
  the whole frame, with the eye's half picked in the uv transform, and
  clamped at the texture's edges, so the other eye bled in where the left
  eye's edges meet it: measured at the first review, 448 of 4096 pixels
  looking back across a left-right frame's seam, and 2652 looking down at
  a top-bottom frame's nadir in a magnified view, since the pole guard
  picks the coarsest level there. The plan's note that a top-bottom
  source's coarsest level can blend one row across the eyes (section
  4.3) no longer applies.
- **The CPU reference** (`render::reference`) is the projection in f64,
  step for step, storing each mip level and the linear target as the GPU
  stores them (an 8-bit level as the exact mean of the codes to the
  nearest code, `reference::halve_codes`, and the nearest f16, both with
  ties to even, which the shaders round to themselves). The Tier 1 tests
  compare `render_still` with it on lavapipe.

Where this differs from the plan, from the evidence:

- The `Source` uniform's members are ordered by alignment, and it adds
  `chroma_scale` beside `chroma_offset`: chroma uv = luma uv · scale +
  offset, with scale = w / (2^s · w_c) and offset = (2^s − 1)(½ − t) /
  (2^s · w_c) for a siting at t between a block's first and last luma
  samples, and 2^s the layout's own subsampling (`PlaneShape::log2_sub`;
  3a's first version took it from the plane sizes, which gave 3 rather
  than 4 for 4:1:1 at a width of 6). The plan's offset alone (½ / W) is
  Left's at even sizes; the scale keeps sizes that aren't a multiple of
  2^s, whose chroma planes round up, aligned away from the ±180° seam,
  and the general form covers every siting. Its `layout` member is
  `kind`, since `layout` is a reserved word in WGSL.
- The `View` uniform holds each projection's constant (`k`: tan(half)
  for rectilinear, half for fisheye, tan(half / 2) for stereographic,
  with half capped as `Lens::half` caps it) rather than the half angle,
  so the shader takes no tangent per pixel.
- Latitude is `atan2(y, |xz|)` rather than `asin(y)`, which keeps its
  precision at the poles in f32; the reference does the same.
- `uv_gradient` has a pole guard the plan's formula lacked: where the
  step is longer than twice the distance from the axis, the ring is
  crossed, so du is ±½ and the radius is regularised, rather than du
  being 0 exactly at a pole and the LOD collapsing there.
- The `Place` uniform of the encode pass has the target's extent, and
  outside it the pass writes black: a `textureLoad` past the texture is
  otherwise implementation-defined.
- The scratch is one texture per plane index, not one per uploader, since
  all of a frame's planes are written before its passes run.
- The StagingBelt is an `UploadPath` of the `Uploader`, not a test-side
  path, so that `bench_upload_5k7` measures the path the trigger would
  switch to, and switching changes nothing else.
- `DebugMode::LodHeatmap` writes luma's λ before clamping in blue, which
  the clamped values in red and green hide.
- The reference returns `Result`: it reads a `VideoPlanes`, and refuses
  a layout the renderer can't sample, as `render_still` does. It also
  has `lod_map` (the heat map) and `render` with a `Placement`.
- Readback copies in bands of at most 64 MiB of padded rows through one
  reused buffer (`readback::BAND_BYTES`). 3a's first readback used one
  buffer for the whole picture, which passes wgpu's default
  `max_buffer_size` of 256 MiB at sizes the device's textures take (a
  16-bit still of 4097×8192, an 8-bit one of 8193×8192) and failed there
  as `gpu_failed`; the first review found it, and a Tier 1 test reads
  8193×8192 back. The device keeps wgpu's default buffer limit, as plan
  section 4.1 has it: an upload's largest plane, 2^27 pixels at 16 bits,
  is 256 MiB exactly.
- `StillRenderer` keeps `render_still`'s pipelines, slot and target for
  more than one still. `Depth::Linear` reads the linear target back as
  it is, for the heat map.

Measured on lavapipe (Mesa's llvmpipe; `crates/render/tests/render.rs`):
`render_still` against the reference is 49.6 to 52.7 dB at 8 bits and
58.9 to 59.0 dB at 10 and 12 bits, through every projection and a morph,
ten plane layouts, both ranges, BT.601, 709 and 2020 and three sitings,
with no channel off by more than 0.1; 89.6 dB at 16 bits; 83.9 to 87.9
dB for the LOD heat map (57.4 to 58.5, 69 and 63 to 78 before the
shaders rounded their f16 stores). Each floor is the measured PSNR less
3 dB. Negative controls: level 0 against trilinear is 22.6 to 43.8 dB,
and one siting against another 19.6 dB, against 52.5 for the right one.
A raised chroma column or row peaks within 0.001 of a luma texel of
where each siting's definition puts it (calibrated with luma, not the
renderer's chroma arithmetic, which the GPU and the reference share: a
mirrored offset passed every other render test). The mips match an area
mean computed independently exactly at 8 bits (its nearest code, ties to
even; to one code until the pass rounded, below), and exactly (its
nearest half) for deep planes unless the mean is within a millionth of a
tie; every 10-, 12- and 16-bit code ingests as
its nearest half; `round_f16` itself returns the nearest half of every
value tested (below); and the linear target holds the nearest half of
every grey level's EOTF. The upload bench's smoke run (640×320, 4 frames) shows a
steady allocator block count on both paths.

Why: as the plan's D29 says. Every format used is guaranteed on every
driver, and `R16Unorm` isn't renderable without adapter-specific
features, which would force a second, compute mip path. Rounded to
nearest, f16 keeps 10-bit within a quarter code and 12-bit within one;
truncated, as lavapipe's own conversion would leave it, the bounds are
half a code and two codes, which is why the shaders round. Capping the chain cuts a 5.7K frame's mip passes
from 23 or 34 to 11 or 16. Analytic gradients remove the seam and pole
spikes implicit derivatives give, and make the result independent of
the GPU's quads, so a CPU twin can check it. Anisotropy is
implementation-defined, so only the GUI uses it. One device function
keeps slice 5b's change to one place.

Limits: a 360° frame whose width isn't a multiple of its chroma
subsampling (an odd-width 4:2:0 frame, which AV1 and VP9 can code but
H.264 and HEVC can't, and which 360° cameras don't make) shows a chroma
step at the ±180° seam: its chroma texture repeats every 2^s · w_c luma
texels, one more than the picture, so chroma jumps half a chroma texel
where U wraps (the first review measured blue jumping 42 codes between
adjacent pixels of a 301-wide frame, against 3 at 300). The
comparison is on lavapipe only until slice 3b's Tier 2 tests; the floors are lavapipe's and may need recalibrating for another
Mesa. 16-bit sources lose their low 4 bits in f16. The goldens'
`passthrough_exact` (plan section 6) can't hold as written for limited
range through an f16 linear target, even with every store rounded to
nearest: flat fields at Y = 86, 159 and 165 come out 81, 166 and 174,
one code from round((k − 16) · 255 / 219), since those k lie within the
f16 error of a tie; full range is exact for every k. D32 revises that
criterion. Anisotropic output is checked only for building. An unchanged frame and view skipping the
projection is the viewer's (slice 7).

Revisit if: the upload bench's trigger holds on huginn or odin (the
belt becomes the default); a driver can't filter `R16Float`; or M7's
colour work needs `R16Unorm` behind the optional features.

*Updated in M1 slice 3b:* the goldens (D32) hold this pipeline to
stored images, the reference and the analytic ideal, with every negative
control, and revise `passthrough_exact`'s criterion. They found that
lavapipe filters `R8Unorm` and `Rg8Unorm` textures to whole 8-bit codes,
which this entry's 8-bit PSNRs already include (D32). The Tier 2 tests
are written:

- `list_adapters_and_features` (`tests/adapters.rs`) notes every Vulkan
  adapter (its `AdapterInfo` with the PCI bus id, the timestamp period,
  the features of the plan's eight formats, its own features, downlevel
  flags and limits) and fails if one can't render into and filter a
  canonical format or copy into a scratch one. Its Tier 1 twin checks
  lavapipe the same way.
- `goldens_on_gpu_match_reference` and `goldens_on_host_llvmpipe`
  (D32).
- `bench_upload_5k7` (`tests/upload.rs`): for 8- and 10-bit 5760×2880
  frames, and an 8- and a 10-bit pair of 3840² frames (the X5's 8K is
  two 3840² streams), each laid out as the pooled download lays out a
  decoded frame (every plane's row rounded up to 256 bytes), both upload
  paths, 300 steps a run, paced two ways: waiting for each step, as a
  frame's display would, for the CPU time per step and the GPU time of
  the ingest and mips; and with two steps in flight, only polling, as
  the upload thread does with the slot ring's two ready slots. Each pace
  counts the steps that made or freed allocator blocks (the plan's
  second trigger), from the allocator's report taken while a step is in
  flight (its uploads returned, the GPU not done) and once it is done,
  with the block counts and the most memory reserved and allocated in a
  note, and a note per input says whether `write_texture` makes or frees
  blocks within a step. Then, on the 5.7K slots, the mips alone and the
  projection at 1920×1080 and 3840×2160, rectilinear 90° and a 300° tiny
  planet. Each row is D20's median and range over `SPHERECUT_BENCH_RUNS`
  runs after a warm-up, and a note says whether the CPU trigger holds
  for each 5.7K input. It runs on the GPU `SPHERECUT_ADAPTER` names, or
  the first one Vulkan lists, and skips without one, or when the name
  matches no adapter or a CPU one; a GPU that is there and refuses a
  device fails it. It needs at most about 1 GB on the GPU. Its Tier 1
  smoke run (640×320 frames, 320² pairs, 3 steps) replaces 3a's.

Where the bench differs from the plan: the pair comes at 10 bits too,
since 10-bit 8K is twice the bytes; the belt is 3a's
`UploadPath::StagingBelt` (above), not a test-side path; and a pair's GPU
row is per frame, since the uploader's timer returns its latest span,
the pair's second frame, which is the first one's size. Two more came
from the slice's review:

- **The allocator is read in flight too.** Once a step is done, wgpu-core
  has freed its `write_texture` staging, and gpu-allocator (0.28) the
  block it made for it, since it frees an emptied block unless it is the
  last of its memory type. Reports taken only then, as the bench's first
  version took them, count growth from one settled state to the next,
  never a block made and freed within a step, which is what the trigger
  names: on lavapipe a waited pair of 3840² NV12 frames settles in one
  64 MiB block and makes and frees a 128 MiB one every step, which that
  version reported as 0. A Tier 1 test
  (`the_bench_sees_a_steps_own_blocks`) shows the in-flight report holds
  a step's staging and, waited, sees the block come and go at every
  step; with two steps in flight the staging block never empties there.
- **The frames have the pooled download's stride.** `write_texture`
  copies a plane in one go only when its rows are as far apart as its
  own staging's, the row rounded up to the device's
  `optimalBufferCopyRowPitchAlignment` (wgpu-core 30, `queue.rs`; 128 on
  lavapipe), and the belt only when they are 256 bytes apart. With tight
  rows, as the bench's first frames had, a 5.7K NV12 plane's 5760-byte
  rows went to `write_texture` in one go and through the belt row by
  row; a decoded frame's 5888-byte rows go the other way round, as they
  now do. P010 at 5.7K and the 3840-wide planes are multiples of 256
  already. The plan (sections 3.5 and 4.4) and D30 said the pooled 256-byte
  stride gives `write_texture` its single copy: that holds only where
  the row is a multiple of 256 already or the device asks for 256
  (amendment 11). Copying row by row costs a memcpy per row of the same
  bytes.

*Measured on odin (RX 7600, RADV, Mesa 26.2.5) in PR #5 at be38fa4 and
PR #6 at 918d031:* `write_texture` makes or frees no allocator block
within a step at any input, so the block trigger doesn't hold. The CPU
trigger held for both 5.7K inputs at be38fa4 (NV12 4.12 ms a step,
against the belt's 17.39; P010 4.80, against 4.78) and holds at 918d031
for P010 alone (4.34, against 4.36; NV12 2.58, against 16.26), each time
through its 4 ms arm, never its 1.5 times, and the belt was never the
cheaper by more than 0.4%. Waited for, the belt's 5.7K NV12 step makes
or frees allocator blocks every time and costs six times
`write_texture`'s; two in flight it costs 3.68 ms, against 2.63. As
written, the trigger makes the belt the default where it costs as much
or more, which isn't what the trigger is for, so the default stays
`write_texture` until H1, which has huginn's numbers too and should
restate the trigger as the belt being cheaper.

*Updated 2026-10-30 by H1 on odin (PR #7, at 43abcda):* the belt was
never cheaper again. One step in flight, 5.7K NV12 costs 2.57 ms a step
with `write_texture` against the belt's 15.84, P010 4.31 against 4.32,
and the 3840² pairs 5.83 against 5.86 (NV12) and 9.26 against 9.31
(P010); two in flight the belt is dearer at every input but the NV12
pair (6.63 against 6.64), and `write_texture` makes or frees no block.
The CPU trigger is now the belt's CPU time under 0.9 times
`write_texture`'s, which `bench_upload_5k7` reports, and the block
trigger stands: the default stays `write_texture`, and huginn's H1 can
still turn the belt on. U2 (the VA surface downloaded straight into a
mapped belt chunk, m1-plan section 4.4) isn't needed on odin either:
its trigger is download plus copy over a third of a 30 fps frame, 11.1
ms, and with the download pool on huge pages (D30) 10-bit's Vulkan
Video download and `write_texture` take 5.0 and 5.9 ms at p50 (8-bit
2.6 and 3.0), on two threads, with nothing dropped. VA-API's own
download, 31 ms a 5.7K NV12 frame and 109 ms a P010 one, is far over
it, but that is the CPU reading the surface across PCIe, which U2
wouldn't remove; slice 5b's zero-copy would.

The review also found `round_f16` untested where it matters least and
inexact where no pass goes. No test reached its subnormal branch (no
10- or 12-bit code gives a half below 2^−14; 15- and 16-bit codes,
filtered edges into black and 12-bit limited range near black do), and
from 65520 up it returned 65536 or more, which isn't a half and which
lavapipe stored as the largest half, where IEEE rounding and
`readback::round_f16` give infinity. It now returns a signed infinity
there, so the store is exact at any magnitude, and a Tier 1 test
(`round_f16_is_the_nearest_half`, `tests/render.rs`) runs it alone on
every finite half of both signs, every midpoint between two of them,
each midpoint an f32 ulp either side and values far past the largest
half, into an `Rgba32Float` and an `Rgba16Float` target, against a
bit-level round to nearest even written in the test: none of the 254
976 values is off, where dropping the subnormal branch puts 6146 off and
dropping the infinity 12. No pass stores such values today, and no
golden moved. The ingest test now ingests every 16-bit code too: codes 1
to 3 are subnormal halves, and 65455 and 65519 are codes an f32 division
would round to the wrong side of a tie. The ingest's own subnormal
branch gives the same halves as its normal one for every code of 16
bits or fewer, so no ingest can show it is needed; it is kept for the
exactness it states.

*Updated after odin's third run (PR #5, at be38fa4):* the encode pass
rounds to its target's codes itself. On the RX 7600 (RADV, Mesa 26.2.5)
`passthrough_exact` found ten limited-range luma codes a code low
against the model of the chain: 74, 129, 135, 141, 153, 165, 208, 214,
220 and 226 (`hw-reports/odin-be38fa409e.md`). RADV exports a
fragment's output to an 8-bit unorm target by packing it to f16 toward
zero (`v_cvt_pkrtz_f16_f32`) before the colour block converts it to
unorm, and a half's step in [0.5, 1) is 2^−11, 0.1245 codes at 255, so
an OETF value that lay less than that above a tie came out below it.
Vulkan allows this: it asks for a conversion to nearest, and requires
only that an exact integer f · L converts to itself.

So `shaders/encode.wgsl` computes round(v · L) in f32 (WGSL's `round`,
ties to even) and returns k / L, with L the override constant `CODE_MAX`
from the output format (`encode::code_max`): 255 for `Rgba8Unorm` and
`Bgra8Unorm`, 1023 for `Rgb10a2Unorm`, and 0, no rounding, for any other
format, which suits a float target. A second override, `HALF`
(`encode::returns_half`), has it return the nearest half of that for
`Rgb10a2Unorm` and `Rgba16Float`. Any other unorm format, or an sRGB
one, is documented as wrong rather than refused, as before. k / L in f32
times L is k in f32 for every k of both depths, so a driver that
converts the f32 itself, multiplying in f32, has an integer, which
Vulkan requires it to keep, if the shader's division was correctly
rounded. WGSL doesn't promise that (it allows 2.5 ulp): divided as a
multiply by f32 1/255, 126 of the 256 codes come out up to 1.9e−5 of a
code off a whole one in f32 (a probe, not kept). A pack to f16 never
leaves an integer. Off an integer, Vulkan asks for the nearest code but
allows either of the two nearest, so nothing the shader returns makes
the store exact on every driver Vulkan's letter allows; what it can do
is keep the value far from a tie, so that a driver that converts to
nearest stores k whatever its arithmetic.
Packed toward zero, k / 255 lies less than 0.1245 codes below k, 0.375
codes or more from either tie. At 10 bits a half's step in [0.5, 1) is
0.4995 codes, and the branch's review found k / 1023 packed toward zero
as little as 0.0005 codes above the tie below k (at k = 1023, an f32 ulp
under 1; with the division's error of up to 3 ulp, 102 codes land within
0.1 codes of a tie), which only an exact conversion to nearest gives
back. Whether RADV packs its 10-bit exports so, as it does its 8-bit
ones, nothing here shows. The nearest half of k / 1023 lies within 0.25
codes of k, and no pack moves it, so the pass returns that. Into
`Rgba16Float` the pass returned the OETF unrounded, so the half stored
was the driver's choice (lavapipe's truncates); it now returns the
nearest half, as every other pass that writes f16 does. 8-bit targets
keep k / 255, exact for a driver that converts the f32. The model of the
chain in `passthrough_exact` now ends with that rounding rather than the
unorm8 store, and its bound is unchanged: the f16 linear target still
moves the OETF by at most 0.0547 codes, and the store no longer adds
RADV's pack. The integer target `PROGRESS.md` proposed (`Rgba8Uint` or a
storage buffer) wasn't needed, and egui's target stays the unorm one it
is.

Tested on lavapipe (Mesa 25.2.8, LLVM 20.1.2):

- `every_code_comes_through_the_drivers_export` (`encode.rs`, no GPU)
  emulates the export for every k at 255 and 1023, and each f32 within 3
  ulp of k / L (WGSL's division may be 2.5 ulp out): what the pass
  returns (k / 255, or the nearest half of k / 1023), unpacked and
  packed to f16 toward zero and to nearest, converts back to k, and lies
  within 0.1246 codes of k at 8 bits and 0.25 at 10, where k / 1023
  packed toward zero comes within 0.001 codes of a tie. Its control
  packs the unrounded values of `passthrough_exact`'s chain toward zero:
  exactly the ten codes odin measured come out a code low (129 is
  131.5735 codes, 131.4844 packed), and none of the 256 full-range ones.
- `the_encode_pass_writes_every_code` (`tests/render.rs`) encodes the
  nearest half of each code's EOTF into `Rgba8Unorm`, `Bgra8Unorm` and
  `Rgb10a2Unorm`: all 256 and all 1024 codes come back as themselves;
  into `Rgba32Float` the OETF comes unrounded, within 1e−5 of the CPU's.
  With the shader rounding to 255 whatever the format, 768 of the 1024
  10-bit codes come out otherwise. Into `Rgba16Float`, the OETFs of
  1024 values come out as their nearest halves (1020 compared, 4 lying
  within a millionth of a tie), where without `HALF` 471 of the 1020
  came out a half below.
- No golden image moved, and none was blessed again: the six stored
  PNGs of `goldens` and the four of slice 4's `pipeline_goldens` are
  identical at the build they were blessed on, `passthrough_exact`
  equals the model at all 512 fields, and `goldens_on_host_llvmpipe`
  passes here. lavapipe's own unorm8 conversion already gave the
  nearest even code to the f32 product v · 255, which is what the
  shader now computes: a probe of 20655 values within 40 ulp of every
  tie, 256 of them with the product on the tie, matched it at every
  value, where rounding the exact product differs at 128.

What this didn't show: RADV itself, until odin's fourth run (PR #6, at
918d031), where `passthrough_exact` gave limited k = 86, 159 and 165
one code off the formula, as lavapipe does, and nothing off the model.
The mip pass then still left its
`R8Unorm` and `Rg8Unorm` levels to the driver, which D32's emulation
found cost the RX 7600's wide views up to 15 dB; it rounds them itself
now (next).

*Amended 2026-10-28: the mip pass rounds its 8-bit means to codes.*
`shaders/mips.wgsl` summed each 8-bit texel's area mean in f32 from the
level above and returned it unrounded, so the store was the driver's
export. A 2×2 mean of codes is a whole code or a quarter, a half or
three quarters past one, so ties are common, and each level is made
from the one above, so a store that breaks them one way moves every
level further that way. RADV's export (above) stored a mean on a half
code, or less than 0.1245 codes above one, a code low: emulated on the
grid, 13674 of luma level 1's 131072 texels (D32 finds this is what the
RX 7600's wide views lost). lavapipe's, the f32 mean times 255 to the
nearest even code, put 3396 of them a code off the nearest code with
ties to even, more of them up than down. Vulkan allows both.

Now the pass works the mean out exactly in u32. Positions are scaled by
the destination size as before, so each overlap is an integer and the
mean is Σ ox·oy·code / (Sx·Sy). At 16384 a side that sum reaches
255 · 2^28, past a u32, so each row's h = Σ ox·code (at most 255·Sx) is
split into q·Sx + r as it comes, Q = Σ oy·q (at most 255·Sy) into
a·Sy + b at the end, and the mean is a + (Sx·b + R) / (Sx·Sy), R being
Σ oy·r, with a numerator under 2·Sx·Sy, 2^29 at most. The pass rounds
that to the nearest code k, ties to even, and returns k / 255, which by
the encode pass's argument (above) a driver that converts to nearest, as
Vulkan asks, stores as k: every f32 within 3 ulp of k / 255, as WGSL's
division may give, lies within 6e−5 codes of k, and packed toward zero,
as RADV packs it, less than 0.1245 codes below k
(`every_code_comes_through_the_drivers_export`). Vulkan's letter alone,
which takes only an exact integer to itself, doesn't promise k.
`reference::halve_codes` computes the same mean in u64 and rounds by the
same rule, bit for bit, where the reference took the f64 mean to the
nearest code with ties up (f64's `round`) before. The f16 path for deep
planes is unchanged: its mean is f32 and `round_f16` gives its nearest
half, as tested above.

Why ties to even: each level compounds the one above, so the rule's
bias matters more than any one texel. Against the grid's chain kept
unrounded (in f64), ties to even puts luma levels 1 to 5 a mean −0.001,
+0.003, +0.002, −0.018 and −0.019 codes off; ties up, the old
reference's rule, +0.106, +0.280, +0.485, +0.631 and +0.743; RADV's
export of the old means −0.105, −0.277, −0.476, −0.615 and −0.742;
lavapipe's +0.025, +0.068, +0.124, +0.147 and +0.200 (a probe made for
this amendment, not kept as a test). It is also the rule of every other
rounding in the pipeline (`round_f16`, the ingest and encode passes,
WGSL's `round`), and the one lavapipe's old stores were nearest to
(3396 of luma level 1's 131072 texels off it, 10612 off ties up), so it
moved the stored images least.

Tested on lavapipe (Mesa 25.2.8, LLVM 20.1.2), in `tests/upload.rs`:

- `eight_bit_mips_are_the_references_code_for_code` makes up to 8
  levels of noise NV12 frames at 256×128, 45×23 and 70×38 and of the
  grid at 1024×512 and 1002×502 (8 of luma and of chroma but at 45×23,
  6 and 5, and 70×38, 7 and 6, where the chain ends at 1×1), and every
  texel of every level of luma (R8) and chroma (Rg8) is `halve_codes`'s
  chain's, code for code. The grid
  at 1024×512 has 27879 ties at luma level 1. Its controls: the old
  pass through RADV's export, emulated, is off at level 1 of every
  frame (13674 texels of the grid's luma), and ties up is off in every
  frame (14008); the old pass through lavapipe's export is printed
  (3396).
- `eight_bit_mips_are_exact_at_the_largest_sizes` makes level 1 of a
  16383×2051 NV24 frame (Rg8 at full size), and 16384×2049 and
  2051×16383 grey ones, every code 128 or more, so each texel's sum
  passes a u32: every texel equals `halve_codes`'s, where the sum in a
  plain, wrapping u32 is off at every one.
- `mips_are_area_means_at_odd_sizes` now holds the 8-bit levels to the
  nearest code, ties to even, of an independent f64 area mean, exactly,
  where it allowed one code.
- The old shader fails all three (at 256×128 a luma texel came out 161
  for 160; 139 texels off at 16383×2051); one summing in a plain u32
  fails the largest sizes at all 8395775 texels; one rounding ties up
  fails all three (1100 texels off at 16384×2049).
- `eight_bit_means_round_to_the_nearest_code_ties_to_even`
  (`reference.rs`) checks `halve_codes` on quarters, halves and odd
  sizes by hand.

Six of the stored goldens and one of slice 4's changed (D32), where a
pixel reads level 1 or more. All nine CPU floors of the render goldens
rose, and of slice 4's four only `h264_full_range`'s (47.2 to 48.17
dB); the other three, and their PSNRs, didn't move. On the RX 7600 at
918d031 (odin's fourth run, PR #6) the CPU checks came out at 58.88 to
60.75 dB, the numbers D32's emulation gives for the fix, above its
prediction (56.11 to 59.78 dB). The pass costs more there: a 5.7K NV12
frame's mips take 1.01 ms of GPU time, against 0.62 ms at be38fa4 when
the stores were the driver's, and a 3840² pair's ingest and mips 0.89
ms a frame, against 0.59. P010's mips, on the f16 path, take 0.51 ms
either way.

## D32. Golden images

*2026-10-24, M1 slice 3b. The M1 plan's D30 (section 6), adjusted to
what was built; it holds the renderer of D31 to stored images, its f64
reference and the analytic ideal.*

What:

- **One test runs every row.** `crates/render/tests/goldens.rs` has a
  function per row of plan section 6's table, and one test, `goldens`,
  runs them in order on lavapipe (`AdapterChoice::Cpu`, with
  `DeviceType::Cpu` asserted), from synth's in-memory planes, with
  `Filter::Isotropic` throughout. It gathers every failure, each naming
  the adapter and Mesa's `driver_info`, and fails once with the list.
  Six rows store an image (`rect_forward`, `rect_ypr`, `fisheye_180`,
  `fisheye_360`, `stereo_tinyplanet`, `morph_half`: 8-bit PNGs in
  `crates/render/tests/goldens/`); the other fourteen check without one.
  The plan's `mips_area` row is slice 3a's
  `mips_are_area_means_at_odd_sizes` in `tests/upload.rs` (45 to 22 and
  23 to 11, R8, Rg8, R16Float and Rg16Float, against an independent area
  mean), which covers it whole. The set is 368 kB, 329 kB of it PNGs
  (the plan guessed 0.5 MB). The decode-to-render goldens through
  `spherecut frame` (`crates/cli/tests/frame.rs`) are slice 4's.
- **The manifest** (`goldens/manifest.json`, schema
  `spherecut.goldens/1`) has an entry for every row, image or not: the
  adapter, driver and `driver_info`, the LLVM version, the wgpu version
  (read from `Cargo.lock`), the commit checked out when it was blessed
  (HEAD, so the commit before the one that adds the blessing), the date,
  the command that regenerates it, the source (pattern, size, layout,
  range, matrix, siting) and the FNV-64 of its planes' rows without
  padding, the view (projection, field of view, yaw, pitch, roll, the
  quaternion, the filter, a morph), the PNG's FNV-64, every threshold,
  and every number the row measured, its controls' among them. A row
  that renders more than one source, or through more than one view,
  records each of them, as a list in the row's order
  (`passthrough_exact` has two sources and eight views), and a row whose
  entry lacks a source, its hash or a view fails.
- **The rules:**

  | Against | Rule |
  |---|---|
  | the stored PNG, at the adapter, `driver_info` and source hash it was blessed at | identical pixels |
  | the stored PNG, another driver build | PSNR ≥ 48 dB, SSIM ≥ 0.995 (BT.709 luma, 11×11 Gaussian), at most 4 pixels with a channel more than 4 LSB off, and none more than 8 LSB off (amended 2026-10-28, below) |
  | a source whose planes hash otherwise | "the synthetic source changed", with both hashes; the render isn't compared |
  | `reference::render_trilinear` (f64) | PSNR ≥ the floor calibrated at blessing (measured − 3 dB, never below 36 dB), on a GPU too (amended 2026-10-28, below); and no channel more than 0.1 off |
  | `synth::render_view_ideal` | over the pixels the analytic LOD puts 2.5 levels down or more, at least 1000 of them: PSNR ≥ the floor calibrated at blessing (measured − 1 dB), and at least 3 dB above the level-0 control |
  | a roll twin (seam and poles) | no pixel more than 2/255 apart; the implicit-LOD control over 100 such pixels |
  | the same picture in the other matrix (`matrix_601_709`) | the BT.601 and BT.709 codings' renders at least 40 dB apart |
  | the same smooth chroma at 4:4:4 (`siting_left_center`) | each siting at least 45 dB from it, and 10 dB nearer than tagged as the other |

  Every other row's thresholds are the plan's (its table), and its
  controls must clear the bounds below. A bound the plan gives as "over"
  is strict: 100 bad pixels, or 10 LSB, fails it.
- **Blessing.** `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p
  spherecut-render --test goldens` runs every row and writes the PNGs
  and the manifest only if all of them pass, every control failing as it
  should; otherwise it panics with "refusing to bless", lists what
  failed and writes nothing. It removes PNGs no row wrote. A floor
  calibrated at blessing goes into the manifest and every later run
  reads it from there; a row whose floor is missing fails with the
  command to run. A blessing compares each floor it calibrates with the
  one blessed before, prints every floor as old → new on the terminal
  (past the test harness's capture), and fails a floor that would drop
  by more than 0.5 dB, naming it and the opt-in:
  `SPHERECUT_UPDATE_GOLDENS=lower` lets it. A failing run writes
  `<row>-<what>-{got,want,diff}.png` (the difference ×16) into
  `golden-diffs/` in the cargo target directory, which each run clears
  first.
- **The harness's own tests**, Tier 1, beside it: the drift rule on made
  images (one LSB everywhere is 48.13 dB, which passes between builds
  and fails at the same one; half a pixel's shift fails; four pixels 5,
  5, 6 and 8 LSB off pass, two channels of one pixel count once, and a
  fifth pixel, or one pixel 9 off, fails with its count and maximum in
  the message); a blessing into a scratch directory with one failed row
  writes nothing, and a clean one then passes a plain run, identical; a
  blessing over a floor blessed 0.4 dB higher goes ahead, over one 2 dB
  higher refuses, names the floor and the opt-in and writes nothing, and
  with the opt-in lowers it; an "over" bound fails at the bound; a
  changed source hash is exactly one failure, the synthetic-source one
  with the driver build in it, and one pixel a step off fails as not
  identical and writes its diffs; the passthrough model and its bound
  (below); the two Tier 2 paths on lavapipe (the other-driver one with
  its golden then changed: one pixel a step off passes it and fails the
  exact rule, five pixels 5 LSB off fail it by the count); and telling
  driver builds apart. Each blesses into a scratch directory of its own,
  which it removes when it passes and keeps, with its diffs, when it
  fails. The Tier 2 paths' tests ran on `tests/goldens` at first, and
  the blessing command runs them beside `goldens`, which rewrites it:
  after a change, they read it halfway through the rewrite and failed
  the command (exit 101) although it had written everything. The
  other-driver rule's fault test (below) read the stored PNGs too, and
  in a blessing compared the images being replaced with the new renders,
  failing the command the same way (the branch's review reproduced it
  with a stored image changed, under a private mount); it now renders
  the views itself. No Tier 1 test but `goldens` reads `tests/goldens`.
- **Tier 2.** `goldens_on_gpu_match_reference` runs every row on each
  GPU Vulkan lists (or the one `SPHERECUT_ADAPTER` names) against the
  CPU and ideal references and the controls only, never against the
  PNGs, and notes each row's numbers for the hw-test report. It skips
  with "no GPU: Vulkan lists only …" when there is none, or when
  `SPHERECUT_ADAPTER` names no adapter there or a CPU one; a GPU that is
  there and refuses a device fails it.
  `goldens_on_host_llvmpipe` runs the whole set on the machine's own
  llvmpipe by the other-driver rule whatever its build, and notes the
  drift. Both hold the rows to the floors lavapipe was blessed with
  (amended 2026-10-28, below: a lower floor for a GPU's CPU checks came
  and went with the mip pass's rounding).

Measured at blessing (lavapipe, Mesa 25.2.8, LLVM 20.1.2; the manifest
has every number; as blessed again on 2026-10-28, after the mip pass
rounded its 8-bit means, below). Each control clears its bound:

| Row | Check | Control (bound) |
|---|---|---|
| the six stored views | CPU 51.15 to 53.25 dB, no channel more than 0.016 off | |
| `rect_ypr` | the red F's centre, the prime meridian at 40° N and the equator at 50° E where the convention's words put them | each angle's sign turned moves F's centre off red: 3 of 3 (3) |
| `fisheye_360` | 7888 pixels past θ = π, all black (`fisheye_180` has none) | |
| `stereo_tinyplanet` | ideal 30.7 dB on 2600 pixels | level 0 23.9 dB, 6.8 dB below (3 dB) |
| `seam_quad` | 0 bad pixels (at most 1 LSB apart) | implicit LOD: 288 (over 100; lodprobe 504) |
| `zenith_quad`, `nadir_quad` | 0 | 186 and 186 (over 100; lodprobe 344) |
| `tinyplanet_pole` | 0 | 178 (over 100; lodprobe 192) |
| `shimmer_tinyplanet` | RMS change 7.3 LSB a frame against the ideal's 19.7; ideal 25.3 dB on 10044 pixels | level 0 changes 5.0 times the ideal's (3), and is 14.3 dB further from it in the worst frame (3 dB) |
| `shimmer_fisheye360` | 2.2 against 30.2; 19.5 dB on 28532 pixels | 3.3 times, 9.3 dB |
| `tenbit_ramp` | P010 and `yuv420p10` identical, no step down, within 0.14 10-bit codes of the CPU | |
| `layout_invariance` | NV12 and I420, each at three strides: 6 renders identical | |
| `passthrough_exact` | full range: every k; limited: as the model says, with 86, 159 and 165 one code off the formula | |
| `range_full_vs_limited` | 10-bit: 1 LSB apart (8-bit: 4) | full decoded as limited: 22 LSB (over 10; 8-bit: 21) |
| `matrix_601_709` | 601 53.3 dB, 709 53.2 dB from the CPU; the two renders 46.3 dB apart (at most 13 LSB) | 601 decoded as 709: 30.8 dB, 19.5 dB under its floor (10 dB) |
| `siting_left_center` | stripes, 10-bit: Left 0.56 and Center 0.52 LSB from the CPU (8-bit: 1.99, 1.76); smooth chroma: Left 51.4 and Center 51.5 dB from its 4:4:4 truth | stripes, Left decoded as Center: 53 LSB (over 10; 8-bit: 55); smooth chroma, each tagged as the other: 33.9 and 34.1 dB, 17.5 and 17.4 dB further (10 dB) |
| `crop_half` | 12096 pixels outside the cover, all black; CPU 54.6 dB | |
| `lod_heatmap` | levels within 0.002 of `uv_footprint`'s | |

Three blessings in a row wrote the same bytes, and the whole test takes
about 7 s here.

Where this differs from the plan, from the evidence:

- **The ideal comparisons cover only pixels at λ ≥ 2.5** (luma's
  analytic LOD before clamping, from `reference::lod_map`). Over a whole
  view, isotropic trilinear isn't nearer the ideal than level 0: on the
  tiny planet's grid it is 28.5 dB against level 0's 29.0, and its
  shimmer frames give a worst margin of 2.3 dB, with level 0 changing
  only 1.33 times as much as the ideal from frame to frame, so neither
  of the plan's margins holds. Where minification is mild, trilinear
  blurs more than the box-filtered ideal (it blends in the next level),
  and level 0's bilinear tap reads most of what lies under the pixel, so
  it neither aliases nor blurs. By threshold, on the tiny planet:

  | λ at least | Pixels | Trilinear | Level 0 | Margin | Shimmer: level 0's change over the ideal's, worst margin |
  |---|---:|---:|---:|---:|---|
  | (all) | 65536 | 28.54 dB | 29.01 dB | −0.47 dB | 1.33 times, 2.28 dB |
  | 1 | 19204 | 29.67 | 28.07 | 1.60 | 1.39, 2.57 |
  | 2 | 5204 | 26.89 | 24.10 | 2.79 | 2.45, 8.12 |
  | 2.5 | 2600 | 30.72 | 23.86 | 6.85 | 5.00, 14.31 |
  | 3 | 1244 | 30.48 | 23.08 | 7.39 | 11.07, 19.32 |

  (the grid with 8 samples a side, which 16 or 32 move by under 0.2 dB;
  the shimmer on the checker with 16, whose pixel counts differ). From λ
  = 2.5, a footprint of 5.7 texels of which level 0's tap reads 4 of the
  32 or more under the pixel, level 0 must alias, and both margins hold
  with room. The 360° fisheye at 192² is minified that far wherever it
  shows anything. A shimmer row compares the pixels minified that far in
  all 8 frames, so that its RMS change measures the flicker of detail
  too fine to show, not the motion of the detail the view resolves. A
  pixel with a corner past θ = π is left out too: it is black in the
  render and the control and partly black in the ideal, and only
  dilutes the comparison (8332 of the fisheye's 36864).
- **The ideal floor is measured − 1 dB.** The plan gave a per-view
  floor without a margin. The ideal is the pattern itself, so the PSNR
  against it moves with filtering, not with a GPU's rounding; a 1 dB
  drop is lost detail or a new alias.
- **Range and siting are asserted at 10 bits, with 8 bits recorded.**
  lavapipe filters `R8Unorm` and `Rg8Unorm` textures in 8-bit fixed
  point: a magnified 0-to-255 edge read back at 16 bits is within 0.035
  of a whole 8-bit code at every pixel, where the f64 reference has
  fractions (121.85, 110.51 and 99.18 against the GPU's 121.99, 110.99
  and 100.00). With the two codings' own rounding on top, an NV12
  full-range and limited-range pair comes out 4 LSB apart (4:4:4 too),
  and the siting stripes 1.99 (Left) and 1.76 (Center) LSB from the CPU,
  past the plan's 2 and 1. Deep sources' f16 planes are filtered in
  floating point, and P010 gives 1 LSB, 0.56 and 0.52. So these rows
  hold P010 to the plan's thresholds, record NV12's numbers, and assert
  the controls on both. Every other row's 8-bit sources are held to the
  PSNR floors D31 measured, which already include this.
- **`passthrough_exact`'s criterion is revised**, as D31 said it had to
  be. Limited range maps k to (k − 16) · 255 / 219, whose fraction is
  r / 219 with r = 36 (k − 16) mod 219, so it never ties; but the f16
  linear target moves a value by up to 2^−11 of itself, which the OETF
  turns into at most (1.055 / 2.4) · 2^−11 of full scale, 0.0547 codes.
  The row now holds that:
  - full range comes out k for every k;
  - every field, in both ranges, equals an independent f64 model of the
    chain (Y′ by H.273, the sRGB EOTF, the nearest half with ties to
    even, the OETF, and the encode pass's rounding to the nearest code,
    which the unorm8 store keeps; D31), or its other candidate where
    f32's error could tip a tie (no field here);
  - limited range comes out round((k − 16) · 255 / 219) except where
    that lies within 0.0547 codes of a tie, and is then at most one code
    off.

  Measured: 512 fields, 0 off the model, full range exact, and limited
  k = 86, 159 and 165 one code off (81, 166 and 174), each 0.0068 codes
  from a tie; 19, 92 and 232 are as near and come out as the formula
  says. A rectilinear view can't put one texel on each pixel across a
  whole equirect, so the fields are 16 texels wide in a 4096×16 NV12
  frame, seen through eight 45° views around the equator at 512×8,
  about a texel a pixel at their centres, so only levels 0 and 1 are
  read; a pixel counts where every tap at both levels lies in one field.
  The model ended with the unorm8 store to nearest until odin's third
  run, where RADV's export, which packs to f16 toward zero first, put
  ten limited codes a code low against it and three of them outside the
  bound; the encode pass now rounds to codes itself (D31), and the
  model, its numbers and the bound are otherwise as they were.
- **Seam and pole placement.** The seam view's yaw (179.55°) puts u's
  wrap between columns 128 and 129, whose centres 128.5 and 129.5 share a
  quad, on every row; the CPU asserts that before rendering. At pitch ±90
  exactly the pole would fall at the image's centre, a corner between
  four quads, so each pole view is turned until the pole is at the
  centre of the quad from (128, 72) to (129, 73) (pitch ±88.90° at
  256×144), or (128, 128) to (129, 129) for the tiny planet (pitch
  −85.28° at 256²), asserted on the CPU to 1e−12.
- **No channel more than 0.1 off the CPU**, beside the PSNR floor, as
  3a's render tests have it: a seam column or a misplaced rim barely
  moves PSNR.
- **The colour rows have truths outside render's colour code.** The plan
  holds `matrix_601_709` and `siting_left_center` to the CPU reference,
  which builds its colour from the same `color::fold` and
  `color::chroma_transform` as the GPU's uniform. With BT.601 and BT.709
  swapped inside the fold, or Left and Center inside `chroma_transform`,
  both rows passed (the siting row with its two numbers traded), and a
  blessing rewrote the PNGs; only the stored images, for the matrices
  `stereo_tinyplanet`'s ideal, and `color.rs`'s unit tests failed (the
  slice's review found it). So `matrix_601_709` also holds the two
  codings' renders to each other: synth codes the same picture with
  core's Kr and Kb, so they agree whatever render does with either, and
  a swap in the fold takes them from 46.3 to 24.6 dB. And
  `siting_left_center` also renders red and blue swinging smoothly
  against each other (32 texels a period), coded at 4:2:0 with each
  chroma sample taken from the pattern where H.273 puts that siting's
  sample, its Y′CbCr written out in the test from H.273, against the
  same pattern coded at 4:4:4, where no siting applies; with Left and
  Center swapped in `chroma_transform`, each siting falls from 51.4 to
  33.9 or 34.1 dB and comes out 17.5 dB further from the truth than
  tagged as the other. Both swaps now fail a plain run, and the
  blessing. The CPU comparison still shares the colour fold: it shows
  the GPU computes what render's colour code says, not that the code is
  right, which these truths, `color.rs`'s unit tests (they code colours
  by H.273's own formulas) and slice 4's swscale cross-check show.
- **A blessing won't lower a floor by more than 0.5 dB unasked.** The
  plan calibrates at blessing; done that way alone, a blessing never
  read the floors it replaced, so only the 36 dB floor and the 0.1
  channel check stood between a worse render and a lower floor. With the
  matrices swapped in the fold, `stereo_tinyplanet`'s ideal fell to
  27.98 dB and failed its 29.71 dB floor in a plain run, and a blessing
  set 26.98 and wrote. At one driver build a floor comes out the same at
  every blessing, so a drop past 0.5 dB is a render that got worse or
  another driver build, and `SPHERECUT_UPDATE_GOLDENS=lower` says it is
  meant; the same swap now makes the blessing refuse on that floor, as
  well as on `matrix_601_709`. Rising floors are written as they come.
- **`rect_ypr`'s markers** are placed from the convention's words (yaw
  turns right about the world's up, pitch looks up about the camera's
  right, roll turns the camera clockwise as seen from behind; +X right,
  +Y up, −Z forward), not from `sphere::orient`, and its control turns
  each angle's sign in turn.
- **The manifest has the checks too**, not only the images, so every
  threshold and control is in one file.
- **Identical pixels, not bytes**: the stored PNG is decoded and
  compared pixel for pixel, so a change in the image crate's encoder
  doesn't fail every row; the PNG's own hash is recorded.
- **`layout_invariance` uses an 1100×550 source**, so that no row is a
  multiple of 64 or 256 bytes already, and asserts that every plane
  really is padded; at 1024 wide both strides would be the rows' own.
  `crop_half`'s 180° panorama is the grid's middle 512×512.
- **`render_view_ideal`** samples the pattern itself at frame 0, n × n
  points a pixel (8 for the grid, 16 for the checker, whose cells a
  minified pixel covers dozens of), with black where the view shows
  nothing; it deals rows to threads in turn, and its result doesn't
  depend on how many there are.
- **On a GPU** the Tier 2 test runs on every GPU Vulkan lists, not one,
  so that a machine with two (a laptop with a discrete card) checks
  both; huginn, odin and the Framework have one each.
- **render and testkit build at opt-level 2 in dev profiles.** The f64
  reference, the LOD map, the ideal and the metrics run per pixel: at
  opt-level 0 the goldens took 16.9 s, at 2 they take 8.0 s (an idle
  machine). Rust has no fast-math, so the results are the same at any
  level and the goldens don't move.

Why: as the plan's D30 says. A golden that passes a broken
implementation is worse than none: at yaw 179.85° or 180° the seam falls
between quads and implicit LOD shows nothing, so each seam and pole row
puts its discontinuity inside a quad and shows, by its control, that it
can see the line. Exact matching catches any change on the reference
box, and tolerances absorb other Mesa builds. The source hash keeps
synth or libm drift from passing as a render change. WGSL's loose
transcendental functions make GPU-to-CPU error grow toward the rim, so
tolerances are measured, not assumed. The metrics are testkit's own,
with no dependencies: dssim is AGPL-3.0, and image-compare pulls in
rayon for two functions.

Limits: the floors, the identical rule and the 8-bit numbers are
lavapipe's, at Mesa 25.2.8 with LLVM 20.1.2. A Mesa update on this box
changes `driver_info`, and the stored images are then held by the
other-driver rule until blessed again. On a GPU the rows are held to
lavapipe's calibrated floors, which a driver's own `atan2` and `asin` may miss near the rim
(the plan's lodprobe measured 40.8 to 55.8 dB, falling toward it); the
report's notes then say by how much. The NV12
range and siting numbers are recorded, not asserted. A blessing writes
the commit and date it was made at, so blessing again on another day
changes the manifest, though no image. The 0.5 dB rule holds a floor
against the one blessed before it, not against the first, so blessings
that each lower a floor by less than that can walk it down; the
manifest's diff shows each step.

Revisit if: a GPU misses a floor in `goldens_on_gpu_match_reference`
(the RX 7600 did at be38fa4, from the mip pass's 8-bit stores, since
fixed: below; at 918d031 it clears every floor by 8.57 dB or more); a
Mesa build
filters unorm8 in floating point (the 8-bit range and siting can then be
asserted); or a control comes near its bound (`shimmer_fisheye360`'s
3.26 times is the nearest, against 3).

*Updated in M1 slice 4*, with the pipeline goldens of plan section 6
(`pipeline_goldens` in `crates/cli/tests/frame.rs`, its PNGs and
manifest in `crates/cli/tests/goldens/`, 320 kB). Each row encodes a
file, runs `spherecut frame` on it (decoding in software, rendering on
lavapipe) and holds the PNG to the rules above: identical pixels at the
driver build and source hash it was blessed at, the other-driver rule
elsewhere, and `frame --reference` on the same file at a floor
calibrated when blessing (measured − 3 dB, never below 36 dB, no
channel more than 0.1 off). The source hash is FNV-64 over the first
decoded frame's planes. Blessing is `SPHERECUT_UPDATE_GOLDENS=1 cargo
test -p spherecut-cli --test frame pipeline_goldens`, with the same
refusals and the same 0.5 dB rule, and a plain run fails if the
manifest lacks a number the run measured. Measured at blessing
(`h264_full_range` as blessed again on 2026-10-28, after the mip pass
rounded its 8-bit means; it was 50.2 dB, 0.016, floor 47.2 dB before):

| Row | Source | Check | Measured (bound) |
|---|---|---|---|
| `hevc10_lossless` | the grid, x265 lossless 10-bit 4:2:0, decoded as `yuv420p10le`; yaw 30, pitch 20, roll 10, 16-bit PNG | stored; CPU | 90.0 dB, 0.0005 at most (floor 87.0 dB) |
| `h264_full_range` | the grid, full-range H.264; 180° fisheye | stored; CPU | 51.2 dB, 0.012 (floor 48.2 dB) |
| `nv12_vs_yuv420p` | the grid as rawvideo NV12 in Matroska, and as `yuv420p` | stored; CPU; the two renders identical | 51.2 dB, 0.012 (floor 48.2 dB); identical |
| `yuv422p10_full_chroma` | flat luma, Cb and Cr swinging ±160 codes from each row to the next, x265 lossless 4:2:2 10-bit; with its 4:4:4 twin and a 4:2:0 coding, which can only hold their mean | stored; CPU; the twin within 1 LSB; the 4:2:0 control over 10 | 62.9 dB, 0.004 (floor 59.9 dB); twin 0 LSB; control 85 LSB |
| `swscale_cross_check` | 4:4:4 pictures of 16-texel flat blocks of 128 in-gamut colours, x265 lossless, at BT.601 and BT.709, limited and full, 8 and 10 bits | frame against swscale (`media::encode::swscale_rgb48`, the matrix and range passed with `sws_setColorspaceDetails`), and against the exact conversion | at most 1 LSB from swscale in all 8 (bound 1); 0.51 to 0.60 LSB from exact (bound 0.75); controls 28 to 30 LSB with the other matrix, 18 to 20 with the other range (over 1); 60352 pixels |

Where this differs from the plan, from the evidence:

- **The swscale cross-check's view and units.** The plan compares
  through "an identity view at level 0"; `frame` has no identity view
  and no way to ask for a level. A 360° fisheye at 512² magnifies the
  256×128 source at every latitude up to 45°, so the analytic LOD is 0
  there, and the check takes only the pixels at |latitude| ≤ 45° whose
  bilinear taps lie inside one block (3 texels from its edge), where
  filtering can't mix colours: 60352 of them. Within 1 LSB holds
  between 8-bit codes, the PNG's against swscale's RGB48 rounded to 8
  bits. Unrounded, swscale's RGB48 was 1.32 to 1.46 LSB from the render
  when the check was built: its fixed-point coefficients are off the
  exact conversion by more than the render's own rounding. So the render
  is also held to the exact conversion, in f64 from H.273's formulas in
  the test itself, within 0.75 LSB (the 8-bit store's 0.5 and the f16
  target's error, D31): a stronger check than swscale's, which keeps
  swscale as an independent second opinion on the matrix and range.
- **The NV12 row's source is rawvideo in Matroska.** A software decoder
  gives `yuv420p`, never NV12, so the NV12 decode is FFmpeg's rawvideo
  decoder reading NV12 planes. The two renders must be identical, as
  the plan says.
- **The 4:2:2 row's check** is the plan's "at full chroma resolution"
  made testable: its render must be its 4:4:4 twin's within 1 LSB (0
  measured), and the 4:2:0 control, which can't hold chroma that changes
  every row, must miss by over 10 LSB (85).
- **The Euler table's pole rows** (plan section 12.1, `the_euler_table`)
  can't be white and black at the centre pixel. Looking straight up or
  down, that pixel is the pole itself, where the equirect's columns
  meet: the analytic LOD there reads a level whose texels span 90° of
  longitude, and isotropic trilinear filtering blends the U disc with
  the grid round it to a neutral grey, 0.72 for U and 0.28 for D, as the
  f64 reference does. The rows assert a neutral centre (channels within
  2/255), above 0.6 for U and below 0.4 for D; and
  `the_markers_round_a_fisheye_turn_with_the_camera` checks the
  markers round the poles in a 200° fisheye looking up and down (B, F,
  L and R where the convention puts them), as well as amendment 10's
  `--roll 90` (U on the left) and `--roll -90`.

Revisit if: a GPU's filter or a change of the LOD rule moves the poles'
greys (the fisheye test's 0.72 and 0.28 are within 0.05), or a swscale
release lands within 1 LSB unrounded, which would let the cross-check
compare RGB48 directly.

*Amended 2026-10-28, after odin's third run (PR #5, at be38fa4,
`hw-reports/odin-be38fa409e.md`).* Two golden tests failed there, for
three reasons, and branch `m1/goldens-hw` settles each: RADV's colour
export moved `passthrough_exact`'s codes, so the encode pass now rounds
to codes itself; RADV's export of the 8-bit mip levels, whose rounding
the mip pass left to the driver, cost the wide views up to 15 dB
against the CPU reference, so the mip pass now rounds its means to codes
itself, exactly, as the reference does (D31), and a GPU is held to
lavapipe's floors (a lower floor for a GPU, from the RX 7600's
numbers, stood in for that fix for a while and is gone); and LLVM 22's
llvmpipe put one stored image 5 LSB off, so the other-driver rule now
counts the pixels past 4 LSB rather than failing on one. The branch's
review found what each passage below marks as its own: a race with
blessing, the encode pass's margins at 10 bits and into f16, a Tier 1
assertion tied to one lavapipe, a test that couldn't tell two rules
apart, and numbers and claims these docs had wrong.

**The encode pass rounds to codes.** On the RX 7600 (RADV NAVI33, Mesa
26.2.5) `passthrough_exact` found 11 limited-range codes one code off
round((k − 16) · 255 / 219), where the model of the chain predicts 3
(86, 159 and 165): 10 came out a code low against the model (74, 129,
135, 141, 153, 165, 208, 214, 220 and 226), and 3 of those (129, 135 and
208, at 131.5753, 138.5616 and 223.5616 codes exactly) lay outside the
bound of 0.0547 codes the f16 linear target allows. RADV exports a
fragment's output to an 8-bit unorm target by packing it to f16 toward
zero first, and a half's step in [0.5, 1) is 0.1245 codes at 255, so a
value less than that above a tie came out below it. Vulkan allows this:
it asks for a conversion to nearest and requires only that an exact
integer f · L converts to itself. So `shaders/encode.wgsl` now rounds to
the target's codes in f32 and returns k / 255, which packed toward zero
lies less than 0.1245 codes below k, so a conversion to nearest gives k
back. For `Rgb10a2Unorm` it returns the nearest half of k / 1023, within
0.25 codes of k however it is packed: k / 1023 itself, packed toward
zero, came within 0.0005 codes of a tie (the branch's review; D31). Into
`Rgba16Float` it returns the OETF's nearest half, as every pass that
writes f16 does. The model of the chain ends with that rounding rather
than the unorm8 store, and the bound is unchanged. A test emulating the
pack on the unrounded values gives exactly the ten codes odin measured
off the model, and none once rounded. No stored image moved and none was
blessed for it: lavapipe's store already gave the nearest even code to
the f32 product, which the shader now computes. Why in the shader, not
in the test's model: with the rounding in the shader a driver that
converts to nearest, as Vulkan asks, writes the CPU reference's codes
whether it packs to f16 first or not, with no integer target; a model widened to
RADV's pack would accept a code low on every driver. Vulkan guarantees
the exact code only where f · L is an integer: in f32 it is for k / L
correctly rounded, which WGSL doesn't promise, and for a packed value it
isn't (D31).

**The wide views on a GPU.** On the RX 7600, at be38fa4,
`goldens_on_gpu_match_reference` measured `fisheye_360`'s CPU check at
44.67 dB, under its 46.59 dB floor, and `fisheye_180` and
`stereo_tinyplanet` at 47.78 and 51.45 dB, under lavapipe's 50.01 and
51.90; every other CPU check came out higher than on lavapipe. PROGRESS
took the losses for the GPU's filtering precision. An emulation says
otherwise: they were what RADV's export made of the mip pass's 8-bit
stores (D31), measured against a reference whose own 8-bit mips ran
high.

`the_wide_views_lose_to_the_mip_stores_not_the_filter` (Tier 1)
renders each CPU check through `Emulation`, the reference with its
filter and its 8-bit mip stores held as a GPU might hold them, rounds
the result to codes as the encode pass does, and compares it with a
reference, as a GPU's CPU check does. At f64 it is the reference, to
1e−12 at every check. The filter can hold its bilinear fractions, its
LOD and its blend weight between levels in fixed point, to nearest or
toward zero, and each filtered 8-bit value to codes. An 8-bit level can
be stored (`MipStore`) as the pass and the reference make it now (the
exact mean of the codes to the nearest code, ties to even, D31), as
RADV's export stores that code, as the reference stored it at be38fa4
(the f64 mean to the nearest code, ties up), or as the pass's unrounded
f32 mean came out of lavapipe's export (times 255 to the nearest even
code) or RADV's (packed to f16 toward zero, then to the nearest code).
The RX 7600 was measured against be38fa4's reference, so the test
measures be38fa4's filters and exports against that one (a filter in
f64 is that reference rounded to codes), and gives every number in this
table, made then, to the hundredth but the lavapipe columns, which are
lavapipe's then. Measured at be38fa4, in
dB (the four filter columns with 8-bit fractions and LOD; RADV's
export with the 8-bit blend; lavapipe's filter with it too, each
filtered value held to codes, and its export):

| Check | RX 7600 | Filter in f64 | Blend 8 | Blend 6 | Blend 4 | Blend 4 toward zero | RADV's mip export | lavapipe | lavapipe's filter and export |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `rect_forward` | 58.55 | 59.16 | 59.10 | 59.06 | 58.23 | 56.98 | 58.79 | 53.14 | 53.53 |
| `rect_ypr` | 58.04 | 58.96 | 58.91 | 58.81 | 57.79 | 55.61 | 58.37 | 53.20 | 53.58 |
| `fisheye_180` | 47.78 | 58.93 | 58.89 | 58.73 | 57.26 | 54.29 | 48.09 | 50.01 | 49.89 |
| `fisheye_360` | 44.67 | 59.98 | 59.94 | 59.79 | 58.13 | 55.52 | 44.84 | 49.60 | 49.01 |
| `stereo_tinyplanet` | 51.45 | 58.96 | 58.91 | 58.74 | 57.03 | 53.86 | 51.74 | 51.90 | 51.96 |
| `morph_half` | 52.71 | 58.93 | 58.89 | 58.79 | 57.68 | 55.19 | 53.03 | 51.68 | 51.70 |
| `crop_half` | 56.57 | 60.81 | 60.75 | 60.62 | 59.24 | 56.59 | 57.00 | 54.19 | 54.27 |
| `matrix_601_709`, 601 | 58.21 | 58.97 | 58.93 | 58.87 | 58.18 | 56.53 | 58.46 | 53.32 | 53.73 |
| `matrix_601_709`, 709 | 58.09 | 59.00 | 58.96 | 58.90 | 58.13 | 56.34 | 58.36 | 53.14 | 53.51 |

- **Filtering precision doesn't explain the RX 7600.** A blend of 8, 6
  or 4 bits costs each check at most 1.93 dB against a filter in f64
  (`stereo_tinyplanet` at 4 bits), the wide views little more than the
  rect views, and comes no nearer than 13.46 dB to the RX 7600's
  `fisheye_360`. A 4-bit blend toward zero, the worst tried, costs up to
  5.10 dB and still misses `fisheye_360` by 10.85 dB, while it puts
  `rect_ypr` 2.43 dB under the RX 7600's. Unrounded, the filter's own
  error is 77.8 to 80.4 dB at 8 bits, 71.8 to 75.8 at 6 and 61.5 to
  65.9 at 4; fewer bits never come nearer the reference (asserted).
- **RADV's mip export does.** The mip pass returned each 8-bit level's
  area mean unrounded and left the store to the driver. A 2×2 mean of
  codes is a whole code or a quarter, a half or three quarters past
  one, and an export that packs to f16 toward zero first puts each mean
  on a half code a code low. Each level is made from the one above, so
  it compounds: on the grid, against the reference's of the time, luma
  levels 1 to 4 came out a mean 0.19, 0.52, 0.89 and 1.13 codes low
  (19%, 51%, 78% and 90% of their texels low), and chroma levels 1 to 3
  0.20, 0.48 and 0.74 low. Part of that was the reference's own: it
  took ties up, so its levels ran high, 0.106 to 0.743 codes on luma
  levels 1 to 5 against the chain unrounded, as RADV's export of the
  old means ran as far low (D31). Against the reference as it is now,
  the old means through RADV's export give `fisheye_360` 49.49 dB and
  `fisheye_180` 51.85, not 44.84 and 48.09 (the test prints each
  check). Emulated, this export comes within 0.43 dB of every RX 7600
  number (asserted within 1 dB; the emulation is 0.17 to 0.43 dB the
  higher), in the same order: below a filter in f64, `fisheye_360`
  loses 15.1 dB, `fisheye_180` 10.8, `stereo_tinyplanet` 7.2,
  `morph_half` 5.9, `crop_half` 3.8 and the rect and matrix views 0.4
  to 0.6. With the filter in f64 it gives the same within 0.05 dB.
- **lavapipe, the same way.** With lavapipe's filter (8-bit fractions,
  LOD and blend, each filtered value held to codes, as found above) and
  its export, the emulation came within 0.59 dB of lavapipe's own
  render at every check at be38fa4; on the mips the pass makes now,
  which lavapipe's export stores as they are, it comes within 0.71 dB
  (asserted within 1 dB), which is the check on the method this machine
  allows. The model is of the lavapipe it was
  made on, so it is asserted only where the machine's lavapipe holds
  what it filters from 8-bit planes to whole codes, as the model has it:
  a probe of the checker magnified 2.25 times, read back in linear
  light, finds every pixel within 0.1 of a code here, where the f64
  reference puts 13493 between codes (asserted). With
  `GALLIVM_PERF=no_aos_sampling`, which filters them in floating point,
  13583 are between codes and the model misses by 8.11 dB (5.53 at
  be38fa4), which the test reports and doesn't assert (the branch's
  review found it failed Tier 1 there). At be38fa4 lavapipe's export
  broke fewer of the ties downward (7.7% of luma level 1's texels a
  code below the reference's, against RADV's 19.4%), and alone, with a
  filter in f64, cost `fisheye_360` 8.4 dB (51.55 dB; its luma level 4
  a mean 0.37 codes low).

**The floor on a GPU.** From be38fa4 until the mip pass rounded its
means, `goldens_on_gpu_match_reference` held each CPU check to the lower
of lavapipe's blessed floor and the RX 7600's measurement less 3 dB,
from a table beside the harness (`GPU_CPU_PSNR`): `fisheye_360` at 41.67
dB, `fisheye_180` at 44.77 and `stereo_tinyplanet` at 48.45, on every
GPU, so a loss of up to 4.92 dB there would have passed. The table, its plumbing
and its test (`a_gpu_is_held_to_the_lower_floor`) are gone: a GPU's CPU
checks are held to the manifest's floors, calibrated on lavapipe, as
before be38fa4, and `the_reference_only_path_runs_on_lavapipe` sees
`rect_forward`, blessed at 99 dB, fail that floor itself.

The emulation's numbers argue for no floor of the hardware's own.
RADV's export of what the pass now returns, k / 255, stores k
in every level of every check (asserted), so RADV's filter as emulated
(8-bit fractions, LOD and blend) puts the RX 7600's checks at 58.88 to
60.75 dB, within 0.06 dB of a filter in f64. At be38fa4 the emulation
missed the RX 7600 by up to 0.43 dB, the emulation the higher. The test
takes that missed squared error as RADV's own (its filter, as far as
the model isn't it), adds it to the emulation's now, and predicts each
check; and, as the control, predicts the old means through RADV's
export the same way, against the same reference (dB; the floors as
blessed after the fix):

| Check | RX 7600 at be38fa4 | Emulated now | Missed then, alone | Predicted | lavapipe's floor | Margin | Old means, predicted | Under the prediction |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `rect_forward` | 58.55 | 59.10 | 71.09 | 58.84 | 50.15 | 8.69 | 58.67 | 0.16 |
| `rect_ypr` | 58.04 | 58.90 | 69.36 | 58.53 | 50.24 | 8.29 | 58.28 | 0.25 |
| `fisheye_180` | 47.78 | 58.90 | 59.34 | 56.11 | 48.2 | 7.91 | 51.14 | 4.97 |
| `fisheye_360` | 44.67 | 59.93 | 58.90 | 56.38 | 48.15 | 8.23 | 49.02 | 7.36 |
| `stereo_tinyplanet` | 51.45 | 58.90 | 63.38 | 57.58 | 49.51 | 8.07 | 54.30 | 3.28 |
| `morph_half` | 52.71 | 58.88 | 64.17 | 57.75 | 49.26 | 8.49 | 54.99 | 2.77 |
| `crop_half` | 56.57 | 60.75 | 66.79 | 59.78 | 51.61 | 8.17 | 57.97 | 1.81 |
| `matrix_601_709`, 601 | 58.21 | 58.91 | 70.65 | 58.63 | 50.34 | 8.29 | 58.42 | 0.22 |
| `matrix_601_709`, 709 | 58.09 | 58.95 | 70.26 | 58.64 | 50.18 | 8.46 | 58.35 | 0.29 |

Every prediction clears lavapipe's floor by 7.91 dB or more, asserted
where lavapipe filters 8-bit planes to codes, as the one the goldens
are blessed on does. One that filters them in floating point renders
the wide views within 0.01 dB of a filter in f64 and calibrates
`fisheye_360` at 56.96 dB, which the prediction misses by 0.58 dB; the
test reports that and doesn't assert it (asserted everywhere, the
prediction failed Tier 1 under `GALLIVM_PERF=no_aos_sampling`).

The floors can't tell the fix from the old mips on RADV: against the
reference as it is now, the old means through RADV's export clear every
floor blessed now, `fisheye_360` by 0.87 dB as predicted (49.02 against
48.15; 49.49 emulated alone), so a Tier 2 run that only clears them
says nothing of the mips. The prediction can: with the old means the
views that read level 1 or more come out 1.81 to 7.36 dB under it, and
the rect and matrix views, which barely do, 0.16 to 0.29. So the control
is held to the prediction: the test asserts that `fisheye_180`,
`fisheye_360` and `stereo_tinyplanet`, the views the RX 7600 lost, come
out more than 1 dB under it with the old means, on any lavapipe, since
none of it is lavapipe's; fed the fix's mips in their place, it fails.
Until the review of the mip fix, the control compared the old means
against be38fa4's reference (44.84 dB for `fisheye_360`) with the floors
calibrated against today's, and these docs took that to show the floors
would catch them; they wouldn't. On lavapipe,
`eight_bit_mips_are_the_references_code_for_code` holds the stored mips
to the reference's code for code, so the old pass fails Tier 1 there. A
floor under lavapipe's would make room for a loss the numbers don't
predict: if the RX 7600 comes in under lavapipe's floors, or more than
1 dB under its prediction, the emulation or the fix is wrong about it,
which is a finding for D31, not a floor to lower.

*Measured at 918d031 (odin's fourth run, PR #6,
`hw-reports/odin-918d03171c.md`):* the RX 7600's CPU checks are the
table's "Emulated now" to the hundredth of a dB at all nine (58.88 to
60.75 dB; `fisheye_360` 59.93, `fisheye_180` 58.90), 8.57 to 11.78 dB
above lavapipe's floors and 0.26 to 3.55 dB above the predictions, the
fisheyes the most (2.79 and 3.55 dB). So the squared error the
emulation missed at be38fa4 wasn't RADV's own: it went with the old
mips' stores or the old encode pass, as the limits below allowed, and
RADV filters the stored codes as the emulation's 8-bit model does, or
nearer f64, which these numbers can't tell apart. Its bands are the
emulation's too (next).

**The error by LOD band.** On a GPU each CPU check also measures its
error over the pixels the view shows, in three bands of luma's analytic
LOD before clamping (`reference::lod_map`): below 1, 1 to 2.5, and from
2.5, the ideal comparisons' bound. Each band's pixels, PSNR and share
of the squared error go into the row's note
(`cpu_lod_1_to_2.5_pixels`, `cpu_lod_1_to_2.5_psnr_db`,
`cpu_lod_1_to_2.5_error_share` and so on). On lavapipe, with the mips
rounded, `fisheye_360`'s error is 58% from 1 to 2.5 (50.18 dB over
17098 pixels) and 42% from 2.5 (50.01 dB over 11878), and
`fisheye_180`'s 90% and 10% (51.29 and 50.36 dB); before, 55% and 45%
(48.84 and 48.18 dB) and 88% and 12% (50.23 and 48.06 dB). RADV's
export of the old means, emulated against be38fa4's reference, put
`fisheye_360`'s 48% and 52% there (44.70 and 42.76 dB) and
`fisheye_180`'s 73% and 27% (49.12 and 42.74 dB); of the codes the pass
returns now, 59% and 41% (58.90 and 58.86 dB) and 92% and 8% (58.90 and
58.92 dB), about level across the bands. The rect
and matrix views lie wholly below 1. The emulation test prints every
check's bands. RADV's at 918d031 are those: `fisheye_360` 58.8% and
41.2% (58.90 and 58.86 dB), `fisheye_180` 92.0% and 8.0% (58.90 and
58.92 dB).

Limits: the emulation is of the reference, not of RADV. It shows the
old means through RADV's export reproduce RADV's numbers at be38fa4,
against that reference, not what RADV's filter holds in fixed point,
which the numbers can't tell apart from f64 (0.05 dB). The prediction
takes the squared error the emulation missed then (58.90 to 71.09 dB
alone) as RADV's own and unchanged by the fix; if part of it came from
the mips, the fix removes that too and the prediction is low, as it
was. Until odin's fourth run nothing here had run on a GPU; that run
measured the fix and RADV's bands (above). Whether RADV packs 10-bit exports toward zero too
is unknown (D31); the deep planes' mips return halves, which no pack
moves.

Revisit if: a GPU's CPU check comes in under its floor, or more than 1
dB under its prediction (as the old mips' stores would at five checks
while clearing every floor; the RX 7600 came in at the emulation's own
numbers at 918d031, above the prediction), or a RADV GPU's bands don't
look like the emulation's (even across the bands, near 59 dB).

**The other-driver rule counts the pixels past 4 LSB.**
`goldens_on_host_llvmpipe` on odin's llvmpipe (Mesa 26.2.5, LLVM
22.1.8) failed `fisheye_180` at 5 LSB against the rule's 4, at 62.79 dB
and SSIM 0.99994; the other five stored images came out 3 or 4 LSB off
at 63.13 to 68.31 dB. One pixel's maximum is no tolerance for a newer
LLVM, and the averages alone miss a fault a few dozen pixels wide. The
rule against a stored image at another driver build is now:

- PSNR ≥ 48 dB and SSIM ≥ 0.995, as before;
- at most 4 pixels (C) with a channel more than 4 LSB off;
- no channel more than 8 LSB (H) off.

The count is of pixels: a pixel counts once, by its largest channel
difference, as `diff_stats` and the seam and pole rows count bad
pixels. A fault is a place in the view, and the same fault puts 1.1 to
3 channels past 4 LSB in each pixel by the colours under it (the tiny
planet's implicit LOD 25 channels in 19 pixels, the pole quads 12 in 4,
the seam 572 in 192), so a count of channels would hold a grey line to
a third of what it holds a red one to. `judge` reports both numbers
(the tiny planet's implicit LOD: "PSNR 53.97 dB, SSIM 0.99972, 19
pixels more than 4 LSB off, max 7 LSB"), every run notes
`golden_pixels_past_4_lsb` beside `golden_max_lsb`, so the next run on
odin records LLVM 22's count, and the manifests' thresholds have
`pixels_past_4_lsb_max` and `max_lsb`.
Both manifests were blessed again through `SPHERECUT_UPDATE_GOLDENS=1`
for those thresholds; their commit and date changed with them, and no
floor and no PNG did. Slice 4's `pipeline_goldens` holds its PNGs to
the same rule, through its own `golden_fault`, which
`stored_frames_follow_the_rules_of_section_6` in
`crates/cli/tests/frame.rs` holds to the rule with made numbers, since
lavapipe here always takes the exact branch.

`the_other_driver_rule_catches_local_faults` (Tier 1) makes faults from
each of the six stored views as rendered, which is its stored PNG
wherever `goldens` passes at the build it was blessed on (the test reads
no PNG, since a blessing rewrites them beside it), or renders them
beside it: moved a pixel right or down (a render, or a crop, a pixel
off), the view a pixel wider or narrower at its edges (a fisheye's rim a
pixel off), the seam's pixels, the poles' quads or the pixels within a
pixel of the rim blacked out, and implicit LOD (the seam and pole rows'
control). Measured here, where those renders are the stored PNGs:

| Fault | Views | Pixels past 4 LSB | Max LSB | PSNR | Fails |
|---|---|---:|---:|---:|---|
| moved a pixel right or down | all six | 6018 to 19979 | 137 to 221 | 24.16 to 26.94 dB | all four parts |
| a pixel wider or narrower | all six (`fisheye_360` can't widen) | 7217 to 13466 | 49 to 171 | 26.26 to 31.80 | all four |
| the seam black | `fisheye_180`, `fisheye_360`, `stereo_tinyplanet` | 128 to 192 | 255 | 28.29 to 33.45 | all four |
| the rim black | `fisheye_360` | 604 | 117 | 29.07 | all four |
| implicit LOD, along the seam | `fisheye_360` | 154 | 100 | 45.87 (SSIM 0.99741) | PSNR, the count and the maximum |
| the poles' quads black | `fisheye_180` (8 pixels), `fisheye_360` (4) | 8, 4 | 204, 162 | 41.66, 43.61 | PSNR and the maximum (and the count at 8) |
| the pole's quad black | `stereo_tinyplanet` | 4 | 95 | 53.62 (SSIM 0.99952) | the maximum only |
| implicit LOD, round both poles | `fisheye_180` | 72 | 9 | 53.41 | the count and the maximum |
| implicit LOD, a minified line at the equator | `stereo_tinyplanet` | 19 | 7 | 53.97 | the count only |
| implicit LOD, the equator's row | `morph_half` | 11 | 6 | 55.06 | the count only |
| implicit LOD | `rect_forward`, `rect_ypr` | 0 | 4, 3 | 57.90, 57.48 | nothing: no seam or pole is shown, nothing is far minified |

The test asserts that every fault fails the rule, but implicit LOD in
the rect views, which it asserts stays within 4 LSB; and that exactly
the four faults within PSNR and SSIM are, with which of the count and
the maximum catches each, so a fault that moves into or out of the
averages fails it. The old maximum of 4 caught those four too; the new
rule catches each by the count or the maximum. With C at 20, or H at
100, the test fails.

The drift, against the same PNGs:

- **lavapipe's own options.** `LP_NATIVE_VECTOR_WIDTH=128` and
  `GALLIVM_PERF` set to `nopt`, `brilinear`, `rho_approx` or
  `no_quad_lod`, alone or together, render all six identically.
  `GALLIVM_PERF=no_aos_sampling`, which filters 8-bit textures in
  floating point, puts them 50.50 to 52.56 dB away, SSIM 0.99863 or
  more, with no pixel past 4 LSB and at most 4; `goldens_on_host_llvmpipe`
  passes under it, and so does `pipeline_goldens`' other-driver path
  with its manifest's adapter renamed so that it applies (51.2 dB, at
  most 3 LSB, no pixel past 4).
- **The views a hundredth of a pixel wider or narrower** at their edges
  (asserted to pass): 57.15 to 61.37 dB, 0 or 1 pixel past 4 LSB, at
  most 5 LSB.
- **A sweep of sub-pixel turns and scales** (a probe, not committed):
  wherever PSNR stayed at 56.8 dB or more, an RMS error twice LLVM 22's
  on `fisheye_180`, at most 1 pixel past 4 LSB and at most 5 LSB; at
  about a fiftieth of a pixel, 51.5 to 56.2 dB, 0 to 92 pixels past
  and up to 8 LSB.
- **LLVM 22 on odin:** at most 5 LSB at 62.79 dB (`fisheye_180`), and
  3 or 4 at 63.13 to 68.31 dB on the rest. Its count wasn't recorded:
  at least 1, and by its PSNR (an RMS error of 0.18 LSB) at most 151
  pixels 5 off; the drift made here at 57 dB or more never put more
  than 1 past 4.

So H = 8 is 3 LSB over the 5 of LLVM 22 and of the drift made here, and
under every fault's maximum but the tiny planet's and the morph's
implicit LOD (7 and 6), which the count catches. C = 4 is over the
drift's 1 and under every fault the averages miss, the smallest of them
`morph_half`'s 11 pixels; the faults smaller than that, a pole's quad of
4 pixels, are 95 LSB or more off and fail the maximum. No C and H
separate everything: a fault within 4 pixels and 8 LSB passes, such as
implicit LOD at one pole quad alone, or up to 4 pixels blacked out where
every channel is under 9 codes. In the darkest pixels the fisheyes, the
tiny planet and `rect_ypr` show, the brightest channel is 12, 15 and 52,
so a pixel blacked out there fails. `rect_forward` and `morph_half` have
dark labels: 1666 and 127 of their pixels have no channel over 8, 1170
and 56 of them black already and 171 and 28 with their brightest at 5 to
8, so up to 4 of those blacked out would be 5 to 8 LSB off and pass.

Why: the averages hold the whole image and the count holds a place in
it. The drift made here is a pixel or two near a rounding, at 57 dB or
more (LLVM 22's, at odin's fourth run, none at all); the faults the
averages miss are lines and spots of 4 to 72 pixels, and each is more
than 4 pixels or more than 8 LSB.

Limits: the faults are on the grid, the stored images' only pattern,
made by editing a stored view's render or by this renderer's own debug
mode, not by another driver. C rests on the drift made here and on
LLVM 22's, which odin's fourth run (918d031) noted as no pixel past 4
LSB in any stored image, at most 4 LSB off, at 63.17 to 68.39 dB. A
fault within 4 pixels and 8 LSB passes.

Revisit if: a run notes more than 2 pixels past 4 LSB for another
llvmpipe (LLVM 22 put none there at 918d031), which would leave C less
than twice the drift; or a driver
build that renders the rows right fails the count or the maximum.

## D33. View conventions

*2026-10-30, M1 slice 7. The M1 plan's D31 (section 7.4), as built:
`sphere::orient` has held the convention since slice 3a, and slice 7
adds the drag, the zoom and the projection switch on it, in
`sphere::grab` and the app's `view` module.*

What:

- **Orientation.** yaw > 0 turns right (from −Z toward +X), pitch > 0
  looks up, and roll > 0 rolls the camera clockwise as the viewer sees
  it, so the horizon turns counter-clockwise: q = R_y(−yaw) · R_x(pitch)
  · R_z(−roll), and world = q · view. At |pitch| = 90°, `to_ypr` puts all
  the heading into yaw. The GUI, `frame`'s flags, M2's keyframes and the
  MCP tools all use it.
- **The drag** (`grab::start`, `solve_level`, `solve_free`). A press
  records the world direction under the pointer and the roll. Dragging
  keeps the roll and solves for the pitch and yaw that put that
  direction under the pointer: the pitch root nearest the current pitch,
  clamped to ±90°. Near a pole, where no orientation with that roll
  reaches it, the closest one is taken and the fit is `Clamped`. With
  Ctrl the drag is free: the smallest rotation, which may roll.
- **Zoom keeps the direction under the pointer** (`grab::zoom_about`).
  Scrolling divides hfov by exp(0.0015 · points scrolled). egui's
  `zoom_delta` (Ctrl+scroll, a touchscreen pinch) divides it by the
  delta. + and − step by 1.15 about the view's centre. hfov stays
  between 10° and the projection's maximum. Zoom input is dropped while
  a switch is under way, which sets the field of view itself.
- **Pointer to lens point.** The pointer's points times the pixels per
  point, less the viewer's origin in whole pixels, go through
  `sphere::pixel::to_lens` at pixel centres, over the same rect the paint
  callback draws into (D36). The point under the pointer is the pixel the
  shader drew there, at any scale.
- **The presets** (Rect, Fisheye and Tiny planet; buttons and keys 1, 2
  and 3) morph the lens over 250 ms with smoothstep, and slerp the
  orientation with it. Rectilinear eases to 170° at most, and fisheye to
  180° at least. Tiny planet is stereographic 300° looking straight down
  (pitch −90°, roll 0), and leaving it levels the view. A drag cancels a
  switch under way. A switch started during another starts from the
  nearer of the two projections the lens is blending, so the picture
  jumps once: a lens blends two projections, not three. The View menu's
  slider sets the morph directly.
- **Metadata.** sv3d crop bounds are applied (D31). An sv3d pose is read
  and shown in a notice, not applied. A stereo source shows its left
  eye.

Why: one convention serves the UI, `frame`, M2's keyframes and the MCP
tools. A level horizon is what people expect when they drag 360° video.
Mapping the pointer in physical pixels, with the callback's own rounding,
makes "the point under the cursor stays under it" exact at fractional
scales. Without the crop, padded or partial equirects would be
misprojected. The Spherical Video V2 wording leaves the pose's direction
ambiguous, so applying it waits for a tagged file checked against
YouTube in M2.

Tested:

- `sphere::grab`'s proptests, 10 000 cases by default and 100 000 in
  the slice's check, over random orientations, projections, morphs,
  fields of view and points. An `Exact` fit puts the direction within
  1e-9 rad of the pointer's and its lens point within 1e-6. A `Clamped`
  fit has no closer pitch on a 0.01° grid around it. The roll is kept to
  1e-9, at pitches more than 1e-4 rad from a pole (at the pole
  `Ypr::from_quat` folds the roll into the yaw). `zoom_about` keeps the
  direction under the point.
- kittest on lavapipe (`crates/app/tests/viewer.rs`):
  - the marker drag at 1 and 1.25 pixels a point: press on the red F
    marker's centroid, drag 30 by 18 points in 6 moves and let go. The
    centroid in the rendered image lands 0.09 and 0.34 px from the
    release point (the limit is 0.5), and the app's q equals
    `solve_level`'s to 1e-9;
  - zoom by the wheel and by `zoom_delta` keeps the direction under the
    pointer within 1e-9;
  - the switch takes 15 frames at 60 Hz, and ends on the preset's lens
    alone.
- The drag test looks through the fisheye. An equidistant lens moves a
  disc's image nearly rigidly, but rectilinear's stretch away from the
  centre moved the 8° disc's centroid 0.7 px off the grabbed point over
  the same drag, though the grabbed point itself lands exactly.

Revisit if: M2's keyframes need a convention for roll at the poles that
`to_ypr` doesn't give, or a tagged sv3d pose file arrives.

## D34. The playback engine

*2026-10-28, M1 slice 5. The M1 plan's D32 (section 8), adjusted to
what was built. Slice 5 lands it in parts. The first part is the
clocks, the UI thread's guard, the frame cache, its budget and the
numbers the overlay reads. The second is the engine: the `Player`, the
decode workers with catch-up, the upload thread and its GPU slots, the
presenter, shutdown, and the seam that slice 6's audio fills. The third
is the Tier 2 benchmarks that play through it on hardware, in H1; what
hw-test does for them is D37's.*

What:

- **Clocks** (`core::clock`). A `Clock` gives the media time at an
  `Instant` (`time_at`). The engine starts, stops and sets it (`play`,
  `pause`, `set`) at the instant that happens. Every method returns at
  once, from any thread.
  - `SystemClock` (paused at 0, or `SystemClock::at(time)`) keeps an
    anchor: an exact time, the instant it was shown at, and whether it
    runs. While it runs it adds the monotonic nanoseconds since the
    anchor as an exact fraction of a second, rounded to a nanosecond
    only where the exact sum doesn't fit `Time`'s `i64` rational. An
    instant before the anchor shows the anchor.
  - `ManualClock::new(frame)` ignores instants. It moves only by
    `advance(by)` or `step(frames)`, and only while it runs, so a test
    or M2's export steps it frame by frame. A sum past what a `Time`
    holds leaves it where it was.
  - Both keep their state in a sequence lock made of atomics, in safe
    Rust. A writer takes it with a compare-and-swap, and a reader
    retries while a write is under way. The UI thread reads the clock
    every frame, and never waits on a mutex.
- **The UI thread's guard** (`core::blocking`).
  - `mark_ui_thread()` marks the calling thread with this build's
    policy, `OnBlock::for_this_build()`. That is `Panic` with debug
    assertions (debug builds and `cargo test`), and `Log` in release,
    which counts every call (`blocked_on_ui_thread`) and logs the first
    with `tracing::error!`.
  - Test harnesses call `mark_ui_thread_with(OnBlock::Panic)`, so that
    a test built in release mode panics too.
  - `may_block(what)` is `#[track_caller]`. On a marked thread it
    panics with "<what> would block the UI thread (at file:line:col)",
    or logs that. On any other thread it costs one thread-local read.
    Playback's lock wrappers (the status lock, the cache's and a
    slot's) and the public methods that lock through them are
    `#[track_caller]` too, so the line named is the caller's, which a
    release build's log needs to say which UI path blocked.
  - It is called, so far, from these blocking helpers:
    - core's `fs`: reading links and metadata, creating and persisting
      a file;
    - media: opening and probing a file, opening a hardware device and
      a Vulkan decoding device, seeking and decoding (`FfmpegVideo`,
      `VideoDecoder`), reading MP4 boxes and the trailer's magic,
      `inject`, `Mp4Writer`'s create, pushes and finish, the Matroska
      remux, doctor and its distro check, and the render node lookup in
      sysfs;
    - render: `Gpu::headless`, and the banded readback, which holds the
      `Device::poll` with a wait and the `recv` of the map's result;
    - synth: the synthetic decode, and the gated backend's wait;
    - playback: the frame cache's lock and its waits, reading the
      memory available, the player's status lock and the other locks
      its threads share, a GPU slot's lock, a decode worker's wait for a
      request (the channel `recv` of plan section 8.6), the upload
      thread's wait for the GPU at shutdown, and `Player::join`.
  - The `Player`'s UI side uses only `try_lock`, `try_recv`,
    `try_borrow` and atomics, so it calls none of them.
- **The frame cache** (`playback::FrameCache`):
  - **Contents.** It holds `Arc<VideoFrame>`s, which it shares with the
    upload thread, so no plane is copied. They are keyed by (stream,
    clip time) in a `BTreeMap`. Bytes are counted from each frame's
    `VideoFrame::bytes()`, its buffers' real sizes, padding included.
  - **Runs.** Frames are linked by (stream, run, place), from D29's
    numbering, so the frame after or before a held one is known without
    a decode (`next_after`, `prev_before`, `following`, `run_end`).
    `frame_at(at)` answers only when the frame linked after the latest
    one at or before `at` is later than `at`, since only then can no
    undecoded frame lie between.
    - A frame offered at a time already held, from another run, is
      `Known`: the held frame stays, and the new run links to it too.
    - Two frames of one run at one time (a broken mux whose timestamps
      tie) keep the later one, and the run's links pass over the tie.
    - A (run, place) that comes again at another time is a decoder that
      numbered its runs afresh: the old link goes, so no false link is
      made. `forget_runs(stream)` drops a stream's links when a new
      decoder opens on it.
    - An evicted frame takes only its own links with it. The links
      between the frames that are left stay.
  - **Policy.** The playhead (`set_playhead`, or `try_set_playhead` on
    the UI thread) splits each stream's frames into four groups:
    1. the *current* frame, the latest at or before the playhead;
    2. the frames *behind* it;
    3. the *window*: the frames after the playhead, nearest first, as
       long as they are within `ahead` (the decode-ahead target, which
       the plan's `PlayerConfig` sets to 1 s) of the playhead and their
       bytes are within
       the budget's half split evenly between the streams offered
       frames since the last `clear`. The nearest frame ahead is always
       in the window;
    4. frames ahead past the window.
    - The current frame and the window are never evicted.
    - Over budget, the frame farthest behind its stream's current frame
      goes first, across all streams. Only when no stream has a frame
      behind does the frame farthest past its stream's window go.
  - **Back-pressure.**
    - A frame ahead goes in only if it would be in the window.
      `insert` waits on the cache's `Condvar` until it would be (the
      playhead moves, the budget grows), until its `StopToken` stops,
      or until `close()` is called. It wakes every 20 ms to look at the
      token, and returns `Stopped` when that stops.
    - `try_insert` returns `Full` instead of waiting.
    - `wake()` wakes the waiters after a seek has stopped their tokens.
    - A frame at or behind the playhead never waits, since playback
      waits for it. It is `Dropped` when evicting every frame farther
      behind still leaves no room.
    - `wait_for_change(seen, timeout)` waits for the cache's
      `generation` to move, for the upload thread.
  - **Pins.** `pin(stream, frame, stop)` puts the frame a request
    landed on in, past the window if it must be, and keeps it from
    eviction until `unpin` (the upload thread's, once the request is no
    longer the target: the presenter has put every stream's landing up,
    or a newer request has come), the stream pins another, or `clear`. Every landing goes in
    that way. Without it a keyframe seek back with the cache full lost
    its keyframe to the frames decoded on from it (after a seek back
    all the history is ahead, so the keyframe is the frame farthest
    behind), and the presenter waited for it for good. A pinned frame
    may hold the cache past its budget, by a frame a stream at most.
  - **Frames alive.** The budget counts the frames the cache holds.
    The frame a worker decoded and waits to insert, and frames evicted
    while the upload thread or a worker still holds them, are alive
    beside them, about a frame a stream more: in the 256 MiB 5.7K run
    below, 261 MiB of frames were alive at worst with 237 MiB held, and
    with the X5's two 8K streams it is about 88 MB. Auto's quarter of
    the memory available leaves room for it.
  - **Too small.** Eviction stops when only current frames and windows
    are left, even past the budget, so a budget too small for a
    stream's current frame and the nearest one ahead still holds both,
    and playback goes on. `CacheStats::too_small` (the "frame cache
    too small" notice) is set while the budget is under `MIN_FRAMES`
    (6) frames: 6 × the sum, over the streams, of the bytes of each
    stream's last frame offered.
  - **The UI thread** uses only `try_set_playhead` and `try_stats`,
    which never wait for the lock and say when it was taken. Every
    other method calls `may_block` before it locks, naming its caller.
- **The budget** (`playback::budget`):
  - `auto_budget(meminfo, cgroup_max, cgroup_current, cgroup_stat)` is
    pure, over the files' text: clamp(¼ × `MemAvailable`, 512 MiB,
    8 GiB), then capped at ½ × (`memory.max` − the working set). The
    working set is `memory.current` less `memory.stat`'s
    `inactive_file`, the page cache the kernel reclaims before it holds
    a cgroup at its limit, as Kubernetes counts it. A container reading
    a large file sits near `memory.max` on page cache alone: 4 GiB with
    4.29e9 bytes current left a budget of about 2 MiB before.
    - `memory.max` reading `max` is no limit.
    - Text that can't be read says nothing: no `MemAvailable` (or one
      not in kB, or not a number) leaves the 512 MiB floor, and a
      `memory.max` that isn't a number is no cap. A `memory.current`
      that can't be read counts as 0, and a `memory.stat` without a
      readable `inactive_file` as no page cache.
    - The cap applies after the clamp, so it can go below the floor,
      and to 0 when `memory.current` is past `memory.max`. The cache
      then holds only the minimum the "too small" rule keeps.
  - `read_auto_budget()` reads `/proc/meminfo`, the `0::` line of
    `/proc/self/cgroup`, and `memory.max`, `memory.current` and
    `memory.stat` of that cgroup and each of its ancestors under
    `/sys/fs/cgroup`, up to the
    mount's root. The tightest room counts, since every ancestor's
    limit applies (a Flatpak's limit can sit on its parent's slice). In
    a container with its own cgroup namespace (Docker and Podman on
    cgroup v2) `/proc/self/cgroup` reads `0::/`, and the container's
    limit is the root's; the host's root has no `memory.max`.
  - `CacheBudget` is `Auto` (the default) or `Bytes(n)`.
    `CacheBudget::parse_mib` reads `--cache-mb`'s and
    `SPHERECUT_FRAME_CACHE_MIB`'s values: `auto`, or a whole number of
    MiB above 0. `CacheBudget::choose` takes the flag first, then the
    environment, then Settings. An environment value that can't be read
    is passed over and returned (`env_error`) for the caller to warn
    about.
  - `budget::recheck(configured, now, held)` is what the upload
    thread's 5-second recheck sets: what `configured` stands for, with
    the `held` bytes the cache holds counted back into `MemAvailable`
    and into the cgroup's room, since both readings count them as used.
    Without that, Auto shrank at each recheck as the cache filled, and
    evicted as it went: odin's 6 GiB settled near 4.8 GiB, and a 4 GiB
    container's 1.5 GiB at 1 GiB.
    - **Under pressure** it gives memory back. What the cache and the
      rest have together is `MemAvailable` with the cache's bytes, or
      the cgroup's room with them where that is tighter (in a
      container `MemAvailable` is the host's). The cache holds no more
      than leaves 1 GiB of that free, and no less than ¼ of it, below
      the floor and below an explicit size too; `set_budget` evicts
      behind frames first.
    - What the two have together doesn't change as the cache evicts,
      so each recheck sets what the one before did. The first rule, ¼
      of it once less than 1 GiB was available, swung: with 1.4 GiB
      available before playback Auto went 512, 358, 512, 358 MiB every
      5 s, evicting about 150 MiB each time (the re-review's finding).
      Now it holds 410 MiB. A set 30 GiB with 24 GiB available holds
      23 GiB, where the first rule let the cache fill memory and then
      cut it to a quarter.
- **`PerfStats`** (`playback::perf`), the one source of the overlay's,
  the smoke run's and the benchmarks' numbers (plan section 10):
  - stages (`Decode`, `Download`, `Convert`, `UploadCpu`, `UploadGpu`,
    `RenderCpu`, `RenderGpu`, `UiFrame`, `ShownInterval`, `AvOffset`)
    keep their last 256 samples in milliseconds;
  - counters (`FramesDecoded`, `FramesUploaded`, `FramesShown`,
    `Dropped`, `DecodeDropped`, `Hitches`, `Seeks`, `SeeksSuperseded`,
    `CacheWaits`, `AudioUnderruns`) are `AtomicU64`s;
  - `summary` gives the count recorded, the last sample, the mean, p50,
    p95 and p99 by the nearest rank, and the maximum. `snapshot` gives
    every stage's summary and every count.
- **Threads** (plan section 8.1). Each is named, and its `JoinHandle`
  goes on a channel that `join` reads, so the threads a thread starts
  are joined too:
  - the UI thread, the caller's, which sends commands on unbounded
    channels and reads with `try_lock`, `try_recv` and atomics;
  - `upload`, started by `Player::new`;
  - `open`, one per open, which ends once it has started the workers;
  - `decode-v{i}`, one per video stream opened.
- **Opening** (`Player::open`, or `open_streams` for several video
  streams). Every open bumps the player's epoch and queues an exact seek
  to 0 on each stream's channel, so the first frame comes as any seek's
  does. The open worker then:
  1. picks each stream's decoder (below);
  2. opens the file's audio through the seam (below), with a token that
     a newer open or shutdown stops;
  3. under a lock that keeps opens in order, and only if no newer open
     has started, clears what the file before left in the cache and the
     slots, resets the status, and starts the workers.
  - Each worker opens its own decoder, with the player's shutdown flag
    as its interrupt, so `begin_shutdown` ends a blocking read.
  - A worker of a file closed since writes no status and sends no
    event, and its frames can't reach the next file's cache:
    `FrameCache::try_insert_until` checks the open's token under the
    cache's lock.
- **The decoder** (`playback::decoder`, amendment 9).
  `DecoderPreference` is `Auto` (the default), `Software` or
  `Only(kind)`. `Only` fails rather than fall back, as `frame --decoder
  vaapi` does.
  - Under `Auto`, `doctor::check_format` maps the stream's codec, depth
    and chroma to the doctor check that covers it: H.264 High, HEVC
    Main, HEVC Main 10 or AV1 Main, all 4:2:0. The kinds whose check of
    it works then decide:
    - none: software, saying why;
    - one: that kind;
    - both: Vulkan Video on a discrete GPU, where VA-API's copy out of
      video memory was the slower on odin, and VA-API otherwise.
  - Hardware falls back to software (`Fallback::Software`). A format no
    check covers, or a file probe can't read, decodes in software.
  - The checks come from `PlayerParts::hw` (the GUI's startup doctor).
    Otherwise the first open that needs them runs them, on its worker,
    and the player keeps them. They run on the GPU's render node
    (`media::hw::render_node` from the adapter's PCI ids), or on
    `PlayerParts::render_node`.
  - The choice and why go in `StreamStatus::choice`. A decoder's
    `FellBack` event goes in `StreamStatus::fallback` and `decode`, and
    out as `PlayerEvent::FellBack`.
- **Decode workers** (plan sections 8.3 and 8.4):
  - **Requests**, one at a time, the latest winning. The UI stops the
    requests before with `StopSource::next()`, then sends, so a seek or
    a wait for room that the next request makes pointless stops at its
    next frame. `StreamStatus::seeks` counts the requests started,
    superseded, decoded and answered from the cache.
  - **A seek** moves the cache's playhead to `at` first, so the frame it
    lands on never waits for room.
    - If a run in the cache holds the frame shown at `at` (`frame_at`),
      that frame answers it, and decoding goes on from the end of the
      run. The decoder is sought to the run's last frame, unless it is
      there already.
    - Otherwise the decoder seeks, and its frame starts a new run.
    - The landing (`StreamStatus::landed`: the request, the time, the
      pts and the `Exactness`) is what the presenter waits for.
  - **A step** follows `next_after` and `prev_before`. Past the links, a
    step forward decodes on from the frame there, and a step back seeks
    to a tick before it.
  - **Decoding ahead**, between requests: frame after frame into the
    cache, with the latest request's token, through the cache's
    back-pressure wait. A frame whose wait a request stopped is kept,
    and offered again when decoding resumes from it.
  - **Catch-up.** While the clock runs and the newest frame decoded is
    more than two frame durations (its own, else the stream's rate)
    behind the clock, the worker calls `catch_up(Some(clock))`. The
    frames passed over count as `DecodeDropped`, by run places, else by
    the time between over the duration. Once a frame at or after that
    time is out, it calls `catch_up(None)`.
  - **End and failure.** At the end the worker sends `StreamEnded` and
    sets the stream's end time. A failure sets the stream's error and
    the request it failed, so the presenter doesn't wait for it, and
    sends `DecodeFailed`. A frame decoded after it (a seek past it)
    clears it.
  - `Decode`, `Download`, `Convert` and `FramesDecoded` come from the
    decoder's own numbers.
- **GPU slots** (`playback::SlotRing`): six slots, of which `gpu_slots`
  (3 to 6, default 4) are used.
  - **State.** A slot's state is one `AtomicU64`: Free, Writing, Ready,
    Shown or Dead, plus a version that every claim bumps.
    - Its frame's fields (stream, epoch, time, pts, run, place,
      duration) are atomics written while it is Writing. They are read
      without a lock, and a read counts only when the word is the same
      before and after it.
    - Its `Arc<GpuFrame>` is in a `Mutex` that the UI thread only
      `try_lock`s.
  - **Claims.** The upload thread claims only a Free slot whose
    `Arc<GpuFrame>` the ring alone holds (strong count 1), so no paint
    callback that sampled it can still be unsubmitted. A slot freed
    while a callback holds it waits, Free, until the callback lets go.
  - **Showing.** The presenter shows a Ready slot by compare-and-swap
    against the word it read (`try_show`). It gives the slot back as
    Ready (`unshow`) when it shows another, so a frame shown before can
    be shown again.
  - **Reuse.** A slot's `GpuFrame` is made for the first frame of its
    shape and reused for every frame of that shape.
  - **Out of memory** (risk 11). A slot the GPU has no memory for is
    Dead for good: the player sends `SlotDropped`, and playback goes on
    with one fewer. That holds while more slots live than streams play:
    two streams need three, since each stream's frame on screen stays
    Shown until its next is up. With as many live slots as streams, or
    fewer, every live slot is Shown and a seek or a step never lands
    (the re-review found two Dead of four stalling two streams). Slots
    die only when the GPU is out of memory, so the viewer (slice 7) is
    to treat a `SlotDropped` that leaves no more live slots than
    streams as a GPU failure.
  - Slots past a smaller setting give their memory back once they are
    Free and unheld.
- **The upload thread** (plan section 8.3):
  - It builds the `Uploader` (the ingest pipelines) and reads the
    budget on its own thread, so `Player::new` waits for neither.
  - **What it uploads**, per stream, as far as its share of the active
    slots (at least 2):
    - while a seek or a step is on its way, nothing until it lands, then
      the frame it landed on. Until every stream's landing is in a slot
      the share is one, so a stream that lands late still finds a slot
      Free; a stream that failed the request, or never opened, is left
      out of the wait. Its landing is unpinned once the request is no
      longer the target, not once it is in a slot: with too few slots the
      presenter shows each stream's as it comes, and a landing gone from
      the cache left its stream wanting nothing, keeping its old frame's
      slot from the stream still waiting (the re-review's finding);
    - otherwise, the frame at the playhead. With fewer than two active
      slots a stream, while any stream's frame at the playhead isn't in
      a slot, the share is one too: on three slots one lens's next frame
      took the last free slot while the other's frame at the playhead
      was still decoding, and with the clock waiting nothing freed it,
      so the lenses showed different frames (found by the re-review's
      fixes' tests: [1, 0] while playing, every run, with the second
      decoder 150 ms a frame);
    - while playing, the frames after it, in order, as far as they
      follow on: linked to the one before, or of another run but
      starting where it ends (decoding goes on from the end of a run
      the cache held in a run of its own). *Updated 2026-10-30 by H1:*
      it took the next frame by time, so after a seek back (each loop
      of odin's real-time benchmark) the frames the cache held from
      before it, ahead of the new playhead past a gap, filled the slots
      while the decoder brought the frames after the seek's one by one;
    - while paused, the next frame, then the one before (by the run
      links), then more frames after.
  - **How.** It uploads one frame per pass, the streams taking turns by
    rank.
    - It frees the Ready slots no longer wanted: another open's, a run
      a seek left, frames passed. A Shown slot is never freed, so the
      frame on screen stays up until the next is Ready.
    - With nothing to do, it waits for the cache to change, 20 ms at
      most.
  - **Uploads** use `upload_placed` with the stream's placement (the
    crop, and the shown eye of a stereo layout), as `spherecut frame`
    does. Each records `UploadCpu`, `UploadGpu` and `FramesUploaded`,
    then asks for a repaint.
  - **Errors.**
    - Out of memory, making or writing a slot, kills that slot.
    - A lost device sets `gpu_error`, sends `GpuFailed` and stops
      uploads.
    - Any other error sends `UploadFailed`, and that frame isn't tried
      again.
  - **Timers.**
    - Every 250 ms it reads the cache's numbers, for `CacheWaits` and
      the "too small" notice (`CacheTooSmall`, once an open).
    - Every 5 s (`BUDGET_RECHECK`) it reads the memory available
      (`read_auto_budget`) and the cache's bytes, and sets what
      `budget::recheck` gives: what the setting stands for, and with
      under 1 GiB available no more than a quarter of it, below an
      explicit size too.
  - At shutdown it waits for the GPU at most 1 s (`PollType::Wait`),
    then closes the cache, which ends every wait in it.
- **The presenter** (`Player::present(now)`), once a UI frame:
  - **The time.** It picks for `clock.time_at(now + latency)`, plus
    `av_offset` while the clock runs: a positive offset shows the frame
    due that much later, video ahead of the audio. Paused, the frame at
    the clock's time shows, so a frame-exact seek or step keeps its
    frame whatever the offset. The latency is the median of the last 32
    UI frame intervals, at most 50 ms.
  - **After a seek or a step** it keeps the frames on screen until
    every stream has landed the request (or failed it) and the frame
    landed on is in a slot. Then it shows them together and sets the
    clock:
    - to the frame's time after a step, a landing after `at`,
      `AfterLast`, `Truncated` or `KeyframeOnly`;
    - to `at` otherwise.
    - Playback, if wanted, starts once each stream has the frames
      after its own Ready too, following on from it: `PREROLL_FRAMES`
      (2), or one fewer than its share of the slots, or as many as
      reach its end; and once the audio is `ready` for
      `PREROLL_AUDIO`. After `PREROLL_LIMIT` (2 s) it starts anyway.
      The state is Seeking until it does. *Updated 2026-10-30 by H1:*
      it started with the seek's frame alone, so a decoder only just
      ahead of real time after a seek (Vulkan Video at 5.7K, bringing
      the frames after a loop's seek every 17 to 33 ms) had each next
      frame come late, and the presenter passed over 4 to 9 at each of
      odin's loop points.
    - Showing every stream's new frame together while the old ones stay
      up takes two slots a stream. With fewer live (three slots for two
      streams, or four with one Dead), each stream's new frame goes up
      as soon as it is Ready, which gives its old slot back for the next
      stream's, and the clock is set once every stream's is up. Without
      that, and the upload thread's share of one while it waits, two
      streams on three slots deadlocked on every seek.
  - **Otherwise** each stream shows the Ready slot with the latest time
    at or before the clock's, which suits variable frame rates.
  - **Counts.**
    - It records `UiFrame` and `Hitches`: an interval over 1.5 × the
      median of the last 32, not of the latency, whose 50 ms cap made
      every frame of a steady UI under 20 fps a hitch.
    - It counts `FramesShown`, and `Dropped`: the frames passed over
      while the clock runs, by run places, else by the time between
      over the duration.
    - It records `ShownInterval` and `AvOffset`.
  - **The playhead.** It moves the cache's playhead
    (`try_set_playhead`) to the clock's time, or to a seek's `at` while
    the seek is on its way. During a step, the worker moves it. When
    the cache's lock is taken, the next `present` tries again, and a
    repaint is asked for so that one comes: until then the upload
    thread picks frames around the old playhead. Whatever drives the
    player presents while it waits on it, as the GUI does every frame
    and the benchmarks do.
  - **The end.** Once the clock passes the first stream's end (its last
    frame's end, else its duration), it pauses on the last frame and
    sends `Ended`. `play` then starts from 0.
  - **Instant steps.** A step of one frame either way is instant when
    the frame next in the run is in a Ready slot, which the upload
    thread sees to while paused. It is shown at once, the clock moves
    there, and no request goes out; so a second step before the next
    present (two key events in one UI frame) goes on from it.
  - **Steps on their way.** A step made while a seek or a step is on
    its way goes on from where that lands: its frames add to a step's,
    from the frames that step started from, and a seek's frame is the
    one it starts from, found as the seek finds it. For a keyframe seek
    that is its keyframe, or the frame shown at its time when the cache
    holds it, since the seek lands there then; the step carries the
    mode on (`Want::Step { mode }`), and so do the steps after it.
    Before, a step started from the frame still on screen and
    superseded the request, so a seek then a step lost the seek; and
    until the re-review a step during a keyframe seek started from the
    time asked, so `seek(45, Keyframe)` then `step(1)` showed 46 where
    the seek alone shows 40.
  - **A step's playhead.** The presenter leaves a step's playhead to the
    worker, which sets it to where the step starts, as a seek's worker
    does, and then frame by frame. It once moved it only frame by frame:
    when the seek's playhead had missed the cache's lock and a step
    superseded the seek before its worker started it, the playhead
    stayed behind, and the frames the step decoded on from a keyframe
    far past it waited for room for good.
- **The `Player`** (`playback::player`), driven from one thread:
  - **Making one.** `Player::new(PlayerParts, PlayerConfig) ->
    Result<Player, PlayerError>`.
    - `PlayerParts { backend, gpu, clock, audio, repaint, hw,
      render_node }`, with `PlayerParts::new(backend, gpu, clock)`
      leaving the rest out.
    - `PlayerConfig { cache, ahead, gpu_slots, decoder, av_offset_ms }`.
  - **Commands**, which return at once: `open`, `open_streams`, `play`,
    `pause`, `seek(at, SeekMode)`, `step(frames)` and `set_config`.
    `set_config` changes the budget and the window at once (on the
    upload thread), the slot count as slots come free, the decoder at
    the next open, and the offset at the next frame.
  - **Reads**, which never wait:
    - `present(now) -> Option<Presented>`: each stream's `ShownFrame`
      (its slot, its frame and its `Arc<GpuFrame>`), whether a frame
      changed, and the time picked;
    - `snapshot() -> PlayerSnapshot`: the state, path, epoch and clock;
      whether it plays, and whether that is wanted; whether it is at
      the end; the frames shown; each stream's status; the audio, the
      cache, the budget, the slots, the counters and the GPU error; and
      whether the threads' part is fresh;
    - `events()`, which is `try_recv` until empty;
    - `config()`;
    - `slots()`, `cache()` and `perf()`, which the viewer and the
      benchmarks read.
  - `PlayState` is Empty, Opening, Seeking, Paused, Playing or Failed
    (every stream failed).
  - Errors are `PlayerError { code, message }`, which carries D27's codes
    from media and render errors.
- **Shutdown** (plan section 8.5):
  - `begin_shutdown`, which `Drop` calls too, returns at once. It sets
    the flag and shuts down both stop sources: the requests' one, whose
    flag is FFmpeg's interrupt, and the opens' one. It sends each worker
    `Close`, pauses the clock and wakes the cache's waiters.
  - `join(budget)` calls it, then polls the threads' handles every 2 ms
    until every thread has ended or the budget is spent. It returns
    `ShutdownReport { joined, panicked, still_running, took }`. Threads
    still running are left detached, and logged.
- **The audio seam** (`playback::audio`), for slice 6:
  - An `AudioOutputFactory` in the parts opens a file's audio on the
    open worker, or says it has none (`Ok(None)`).
  - Its `AudioOutput` gives the clock that video follows from then on
    (its `set` is the audio's seek), and `ready(ahead)` for the
    preroll.
  - Without one, video plays on the parts' clock.
- **The Tier 2 benchmarks** (`crates/playback/tests/tier2_playback.rs`,
  plan sections 12.2 and 12.3, risk 1). Each plays a file through a
  `Player` on the machine's GPU, as the GUI will:
  - `present` and `snapshot` at 60 Hz deadlines, on a thread marked with
    `OnBlock::Panic`, so that any wait there fails the benchmark on the
    hardware too;
  - each new frame projected (rectilinear, 90°, anisotropic) into an
    offscreen 1920×1080 target and encoded into an `Rgba8Unorm`
    texture, recording `RenderCpu` and the GPU's `RenderGpu`. The raw
    view's two lenses go side by side, 960×1080 each, through the same
    projection, since the GUI's raw view comes in slice 7.
  - The decoders open on the rendering GPU's render node (or
    `SPHERECUT_RENDER_NODE`'s), where doctor's checks run once per
    benchmark and are handed to each player.
  - A run is two passes, each on a player of its own. **Unthrottled**,
    on a `ManualClock` moved to the next frame as soon as every stream
    shows the one it is at, through the file once or for 30 s: what the
    engine can do. **Real time**, on a `SystemClock`, playing from the
    start again at the end, a seek every loop: what plays.
  - Each full second gives the frames decoded (per stream) and shown;
    the real-time pass also counts `Dropped`, `DecodeDropped` and late
    presents, a present whose frame had ended before the time it was
    presented for. A note gives every stage's p50 and p95 over the
    real-time pass (from samples gathered as they come, since the
    engine keeps only 256 a stage), the loops, the UI ticks missed and
    each stream's decode path.
  - **The gate**, for `bench_playback_realtime_5k7_8bit` and `_10bit`
    on hw-test's 20 s perf clips (D37), on the hardware kind `Auto`
    picks from doctor's checks: no drop and no decode-drop after the
    first second of 60 s in real time, every unthrottled second
    decoding at least 1.2 × 29.97 frames, and every stream on that
    kind throughout. The rows are recorded before the gate judges, so a
    failing run's numbers are in the report.
  - The other kind, where doctor says it decodes the format, and
    software are recorded, 20 s each, as are `bench_playback_realtime_8k_8bit`
    (synth's 8K perf clip), `bench_playback_x5_raw` (the longest file
    of each X5 recording mode, both lenses) and the stitched samples
    outside the criterion. `bench_playback_stitched_samples` gates a
    stitched equirect of 5.7K at 30 fps or less as the perf clips are,
    with 1.2 × its own rate.
  - A run that only records and fails is a note saying why. Where the
    GPU (a CPU adapter is none), hardware decode of the format, the
    perf clips or the samples aren't there, a benchmark records what it
    can and skips with the reason; `Auto` picking software is a skip
    that quotes its reason.
  - `bench_perf_clips_software_decode` records each perf clip's
    software decode rate, which hw-test holds against "too easy". A
    clip that wasn't made is a row "not measured" with the reason, and
    the others are measured; it skips only when there is none.

Where this differs from the plan:

- **Keyed by clip time, not pts.** The plan keys the cache by (stream,
  pts). Clip time orders a stream's frames as pts does (D29: clip time
  is pts less `origin_pts`, times `time_base`). The playhead, `ahead`
  and the presenter are all in clip time, so the cache compares frames
  with them directly, with no time base. The key is `VideoFrame::time`,
  exact.
- **The window.** The plan bounds the frames ahead by min(ahead × rate
  × frame_bytes, budget / 2). Here the bound is time (`ahead` past the
  playhead), so variable-rate streams need no rate, plus bytes (half
  the budget, split between the streams), counted from the nearest
  frame out. A gap left in the window by an eviction or a seek is then
  filled before anything farther ahead is decoded.
- **Frames past the window can be evicted.** The plan evicts frames
  ahead "only after a seek moves the playhead past them". After a seek
  back, the frames decoded before it are all ahead. Under the plan's
  rule they would hold the budget, the window couldn't fill, and the
  decode worker would wait for good. Here they are evicted, farthest
  first, but only after every frame behind.
- **Over budget, by a minimum.** The plan says only that a budget too
  small "still plays". Here the cache keeps each stream's current
  frame and window even past the budget, evicting everything else
  first. The window's half of the budget bounds it, beyond its nearest
  frame.
- **Frames behind never wait.** The plan's back-pressure covers the
  frames ahead. A frame at or behind the playhead (a seek's landing,
  a step back) is let in by evicting farther history, or dropped.
- **Stop and wake.** The plan's wait has a timeout. Here it is 20 ms
  slices that check a `StopToken`, so a superseded seek's worker stops
  within one slice, and `close()` at shutdown ends every wait.
- **The cgroup cap** can go below the 512 MiB floor, as above, rather
  than being clamped back up: the floor is for small machines, and
  memory a limit doesn't leave can't be used whatever the floor says.
  Ancestors' limits count too, up to the mount's root, which the plan
  didn't say.
- **A recheck counts the cache's own frames as free.** The plan
  re-derives Auto from the memory available every 5 s. Read with the
  cache full, that is a quarter of less each time, and the budget sank
  below the plan's table, evicting at each recheck.
- **Pressure leaves 1 GiB.** The plan shrinks the budget below 1 GiB
  available. Here the cache holds no more than leaves 1 GiB of what it
  and the rest have together, and at least a quarter of that, so the
  budget doesn't swing as the cache's own evictions lift the pressure.
  It applies to an explicit size, and to a cgroup's room, too.
- **A cgroup's use is its working set**, `memory.current` less
  `inactive_file`, so `auto_budget` takes `memory.stat`'s text as a
  fourth argument. The plan's `memory.current` counts page cache the
  kernel would reclaim first.
- **A request's landing is pinned** in the cache while its request is
  the target, past the budget by a frame a stream if it must be. The plan has no
  such rule; without it a keyframe seek back with the cache full never
  landed.
- **PerfStats keeps its rings in atomics.** The plan puts each stage's
  ring in a `Mutex` that the UI thread `try_lock`s. Here each sample is
  an `AtomicU32` (an `f32`'s bits) and the ring's count is a
  `fetch_add`, so recording and reading never lock, and the UI thread,
  which records `UiFrame` and `RenderCpu` itself, can record without a
  second path. A read that overlaps a write may see the sample just
  replaced instead of the new one, which doesn't change an overlay.
- **`Player::new` takes its parts.** The plan's `new` takes the
  backend, an `Arc<Gpu>`, a `SlotRing`, the audio factory, the config
  and the repaint callback, and returns a `Player`. Here
  `Player::new(PlayerParts, PlayerConfig)` returns a `Result`:
  - starting the upload thread can fail, and a `Player` without it
    would never show a frame;
  - the parts add the clock, which the plan had nowhere to give and the
    headless runs need to be a `ManualClock`, doctor's checks if the GUI
    has them, and the render node;
  - the `Gpu` goes by value, since cloning it shares wgpu's handles;
  - the player makes its `SlotRing` from `gpu_slots`, so the ring and
    the setting can't disagree, and lends it out with `slots()`;
  - the audio factory and the repaint callback are optional: slice 6
    brings the first, and a headless run needs neither.
- **`present` returns `Option<Presented>`**, not `Option<Arc<GpuFrame>>`.
  It gives a `ShownFrame` per stream (its slot, its frame's time, pts
  and run, and the `Arc<GpuFrame>`), whether any changed since the last
  call, and the time picked.
  - The X5's raw files hold two video streams, one per lens, which the
    viewer stitches, so one frame isn't enough.
  - A paused viewer that knows nothing changed needn't repaint.
  - Slice 7's paint callback takes `ShownFrame::gpu`. While it holds a
    clone, until eframe has submitted the draw, the slot isn't reused.
- **`open_streams(path, streams)`** opens several video streams, by
  index. `open` is `open_streams(path, vec![None])`, the best one.
- **Seek kinds** are media's `SeekMode` (`Exact` or `Keyframe`): the
  plan's `SeekKind`, under the name it already had.
- **`DecoderPreference::Only(HwKind)`** takes the place of the plan's
  `VaApiOnly`. Amendment 9 makes Vulkan Video a hardware kind like
  VA-API, so either can be asked for alone. `Auto` picks by doctor's
  checks rather than in a fixed order, as amendment 9 asks.
- **`join`'s budget is for all the threads**, not 2 s for each: with
  five threads, a stuck backend would hold the GUI's exit for 10 s.
  Threads still running when it is spent are named in the report and
  left detached, since the process is exiting.
- **The snapshot on contention.** The plan gives the previous copy.
  Here only the part the threads write (each stream's status, the
  budget, the GPU error) is the previous copy. What the UI thread holds
  or reads with atomics is read each time: the state, the clock, the
  frames shown, the slots, the counters, and the cache's numbers when
  its lock is free. `fresh` says whether the threads' part is new.
- **One driving thread.** The `Player` is `Send`, not `Sync`. The UI
  thread's state (the target, the frames shown, the intervals) is in a
  `RefCell` that the `&self` methods borrow, so it needs no lock. A
  call made while another is under way (a repaint callback calling
  back in) returns at once and does nothing.
- **The present latency** is one UI frame: the median of the last 32
  intervals, capped at 50 ms. What vsync and the compositor add can't
  be measured headless; slice 7 can measure it. Hitches are judged
  against the median itself, as the plan says, not the capped latency.
- **The A/V offset applies while playing.** The plan adds it to the
  time picked for, always. Paused, that showed the neighbour of the
  frame a frame-exact seek or step put up, once the upload thread
  brought it up. The setting's doc had the sign backwards: a positive
  offset puts video ahead of the audio.
- **Two streams on few slots.** The plan keeps every old frame up until
  every new one is Ready, which takes two slots a stream. Below that
  (gpu_slots' floor of 3 with the X5's two lenses, or a slot Dead) each
  stream's frame goes up as it comes, and the upload thread's share is
  one slot while it waits, so the late stream finds one. The two lenses
  may then show frames of different times after a seek, until the late
  one's frame is up.
- **Steps add up.** The plan says nothing of a step made while a
  request is on its way. Here it goes on from where that lands.
- **Slots** keep their state and a version in one `AtomicU64`, not an
  `AtomicU32`, so that a slot freed and filled again between the
  presenter's read and its swap isn't taken for the frame it held.
  - The plan's "Released" is a Free slot whose `GpuFrame` a draw still
    holds (`SlotStats::held`). Dead is a slot out of memory.
  - Freeing covers more than a stale run after a seek: it covers any
    Ready frame no longer wanted.
- **The preroll's 2 s limit**, and the switch to `SystemClock` with a
  notice (plan section 8.4, step 6), need audio, and come with slice 6.
  The preroll itself is here, through `AudioOutput::ready`.
- **The preroll waits for video too.** The plan's step 6 resumes once a
  Ready slot holds the target. Here it waits for the frames after it
  as well (above), which cost a seek about two frames' decoding while
  playing and none paused, since H1 showed the target alone dropping
  frames after every seek a decoder only just keeps up with.
- **The hardware device** isn't the player's to choose: the
  `FfmpegBackend` it is given opens the devices. The GUI is to make it
  on the rendering GPU's node (`FfmpegBackend::on_node`), so that
  decoding runs where doctor's checks ran.
- **The benchmarks' unthrottled pass** is the plan's "unthrottled
  per-second minimum" made concrete: a player of its own on a
  `ManualClock`, rather than the real-time player's clock let go, so
  that its seconds count the engine's work and nothing of the 60 Hz
  loop's. The plan's 1.2 × 29.97 is kept for the 30 fps perf clips (36
  would be 1.2 × their own rate), and the stitched samples use their
  own.
- **The benchmarks gate on Auto's pick**, the decoder the GUI will use,
  and record the other kind and software; the plan names "the path the
  backend picks" only. Late presents, the 8K perf clip and the
  software rows are additions. The gated run also fails if a stream
  fell back to software, which would otherwise pass on a fast CPU.
- **`never_blocks`' bound.** The plan asks that every UI iteration take
  under 2 ms.
  - Here each iteration's time is its wall time less the time the
    kernel kept the thread waiting to run, the run-queue wait of
    `/proc/thread-self/schedstat`. The 99th percentile of that must be
    under 2 ms and the slowest under 10 ms, and the wall times' median
    under 2 ms.
  - Measured: with 4 busy loops beside the test on this 4-CPU box, the
    storm's 99th percentile came to 4 ms of wall time (4.4 ms with 6),
    every one of those iterations a preemption. Less the waits to run,
    its 99th percentile stayed between 118 and 189 µs and its slowest
    at 310 µs or under.
  - This box is a VM whose host takes its CPUs away now and then (4543
    ticks of steal in 3 h 51 min), which no run-queue wait shows. The
    10 ms ceiling leaves room for that, and still fails a wait on one
    of the storm's 50 ms decodes.
  - The guard makes a wait on a lock, a channel or a join on the marked
    thread a panic anyway, so the times are there to catch work that
    grows, not waits.

Why: as the plan's D32 says. PLAN requires that the UI never block and
that the cache be bounded and configurable. An egui-free crate is what
M2's export and the benchmarks reuse. `try_lock` turns contention into
one repeated frame instead of a stall. Clippy's configuration is per
crate and can't see into other crates, so the runtime guard, with its
negative control, covers what clippy can't. The cache's two lists of
evictable frames (behind, then ahead past the window) keep what the
next second of playback and a step back both need, and the window by
time is what the presenter asks for.

For the engine, beyond the plan's reasons:

- One request at a time, the latest winning, each with its own token, is
  what keeps a seek storm to one decode.
- The UI reaches the threads only through unbounded channels, the
  requests' tokens and a `try_lock` of the cache's playhead, and reads
  what they did through the slots' atomics and a `try_lock` of the
  status. So no command or read can wait.
- Picking frames by time, not by index, is what makes the presenter
  right for variable frame rates, and the same for every stream.
- Counting drops by run places, where a run says how many frames lie
  between, and by time only where it can't, keeps `Dropped` and
  `DecodeDropped` exact for a constant rate.

Tested:

- `core::clock`: a system clock runs only while playing; a seek shows
  its time exactly; instants before the clock was made work; a sum past
  what's exact rounds to a nanosecond; a manual clock moves only when
  told and running; frame steps land on frame times; a reader never
  sees a torn write from two writers; and a proptest
  (`a_system_clock_adds_up`) that whatever the plays, pauses and seeks,
  the clock shows the time they add up to. Both proptests here pass at
  `PROPTEST_CASES=100000` (the cache's in 181 s).
- `core::blocking`: unmarked threads may block; the negative control,
  `a_marked_thread_panics_when_it_would_block`; this build's policy
  panics in tests; the mark is per thread and comes off; the log policy
  counts and goes on. Media (`the_ui_thread_may_not_open_or_probe_media`),
  render (`the_ui_thread_may_not_open_a_device`) and core's `fs`
  (`the_ui_thread_may_not_write_files`) each check a marked thread
  panics before it touches anything.
- `crates/playback/tests/cache.rs`:
  - frames link by run and place; tied frames keep the later one, and
    links pass over the tie; a decoder that numbers its runs afresh
    makes no false link;
  - eviction takes the farthest behind first, then the farthest past the
    window; the window is half the budget and `ahead`; a budget too
    small still plays and says so;
  - the producer waits at the limit and goes on when the playhead moves;
    a waiting producer stops with its token and at `close`; a change
    wakes a waiter;
  - a pinned frame stays until it is let go
    (`a_pinned_frame_stays_until_it_is_let_go`): evicted neither as the
    farthest behind nor past the window, let in past the window, and
    gone once unpinned, re-pinned or cleared; and over the budget by
    one, a seek back that leaves a frame past the window evicts
    nothing, and that frame offered again goes (`Dropped`);
  - the negative control, `the_ui_thread_only_tries_the_lock`: on a
    marked thread `try_stats` and `try_set_playhead` work, and every
    locking method panics with its own words, naming the test's line;
  - the proptest `the_cache_keeps_its_rules`, against a model, over runs,
    single frames, pins and unpins, playhead moves, budget changes and
    clears on two streams. Bytes and frames are counted exactly and
    never pass the budget but by the pinned frames; the window and the
    current frames are never evicted, except where a new stream or a
    nearer frame changes them; the frames evicted are exactly those the
    model's rules evict, in their order, at every step that evicts;
    `Full`, `Stored`, `Known` and `Dropped` come exactly when the rules
    say; and the links are exactly the shared runs of adjacent frames
    held, through every eviction. With pins it passes at
    `PROPTEST_CASES=100000` in 24 s of a release build, eight runs in a
    row. Before that, runs at that count found the model wrong, not the
    cache, in four ways, each a state only a pinned frame past the
    budget reaches: a playhead move, a frame turned away (`Full`) and
    letting go of a frame not pinned evict nothing, so a cache a move
    leaves over its budget keeps frames the rules would evict until the
    next step that evicts (`check` allows that and nothing else); and a
    frame held already, offered again in that state, can be the one its
    own offer's eviction takes, `Dropped` though it is ahead of the
    playhead. `Insert::Dropped`'s doc said only "behind the playhead"
    and now says that too, and `a_pinned_frame_stays_until_it_is_let_go`
    shows it. The cases are kept in `cache.proptest-regressions`.
  - Eleven faults put into the cache by hand were each caught by the
    proptest: a window frame evicted, the cap taken as the whole budget,
    no eviction, links kept after their frame went, the current frame
    unprotected, a `Known` frame counted twice, the time limit ignored,
    the nearest frame behind evicted first, the nearest frame past the
    window evicted first, frames past the window evicted before those
    behind, and one frame too many evicted. The nearest-behind fault
    passed until the model evicted by the rules itself.
- `playback::budget`: huginn's 128 GB take the cap, 8 GiB or 345
  frames of 5.7K NV12; odin's 32 GB with 24 GiB available take a
  quarter, 6 GiB or 258 frames, as the plan's table has it; a small
  machine keeps the floor; a container's limit caps it, below the
  floor too, and to 0 past its limit; `max` is no limit; malformed text
  says nothing; the first `MemAvailable` line counts; the flag, the
  environment and Settings, in that order; a recheck shrinks under
  pressure; a recheck counts the cache's own frames as its own
  (`a_recheck_counts_the_cache_s_own_frames_as_its_own`: eight
  rechecks, each read with the cache full to the last budget, keep
  odin's 6 GiB, a 4 GiB container's 1.5 GiB and a set 2 GiB; without
  the fix odin's went 6, 4.5, 4.875 … 4.8 GiB); a recheck under
  pressure settles (`a_recheck_under_pressure_settles`: with 1434 MiB
  available before playback eight rechecks keep 410 MiB, where the
  first rule went 512, 358, 512 … MiB; a set 30 GiB with 24 GiB
  available keeps 23 GiB; a set 8 GiB in a container with 3 GiB left
  keeps 2 GiB); the page cache a cgroup would give back is room (a
  4 GiB container at its limit, 3 GiB of it `inactive_file`, takes
  1.5 GiB, and none without `memory.stat`; active page cache is use);
  the tightest cgroup up a fake tree counts, with an ancestor's
  `memory.stat`; a cgroup namespace's limit at the root counts
  (`0::/`, and a cgroup below it); this machine has a budget.
- `playback::perf`: the ring keeps the last samples in order; counters
  add up across threads; percentiles by the nearest rank.
- The engine, on lavapipe and on a `ManualClock` wherever wall time
  would make a test flaky (`crates/playback/tests/engine.rs`, 26
  tests):
  - the presenter shows the latest frame at or before the clock, on a
    real variable-rate H.264 clip through `FfmpegBackend`;
  - frames passed over while playing count as dropped: exactly 20 for a
    clock moved two frames at a time, each frame waited for, then as
    many as were passed over (32 in the last run) while it moves on
    without waiting. A seek or a step while paused drops nothing;
  - a keyframe seek to frame 45 shows the keyframe, 40, sets the clock
    to it, and plays on from it;
  - hitches: 40 presents 80 ms apart, a steady UI under 20 fps, count
    none, and one 200 ms gap counts one;
  - a decoder 2 ms a frame, with no keyframe in reach, whose clock leaps
    from frame 0 to 300 catches up without making the frames it passes:
    the cache holds 0 to the window's last frame before the leap and
    300, and `DecodeDropped` is the frames between. The test asserts
    300 less that frame less 1; the window ended at 30 or 31 by timing
    over the runs, so the count was 269 or 268;
  - a slot is reused only once nothing else holds its frame. With a
    clone held, as a paint callback would hold it, the slot is Free with
    2 holders after twenty frames, and its texels, read back, still
    hold its frame's index. Let go, it is written again. With the strong
    count check taken out of `claim`, this test and the ring's own fail;
  - a seek keeps the frame on screen until the new one is Ready (242 ms
    on a decoder that takes 30 ms a frame), then frees the old run's
    slots, and playback goes on from there;
  - steps of one frame either way are instant, with no request to the
    worker, when the slots hold the frame; longer steps and steps back
    from a seek follow the cache's links, or decode where the cache
    holds nothing; a step of 40 frames past a window of 10 lands, the
    worker moving the playhead as it goes;
  - a step made while a seek or a step is on its way goes on from where
    it lands, on a decoder 30 ms a frame: a seek to 207 then a step of
    1 shows 208, 207 never shown; two steps of 5 go 10; a seek to 230,
    a step of 1 and one back show 230. A keyframe seek to 213 then two
    steps of 1 show 210, from its keyframe 208; before the re-review's
    fix they showed 215. `player::tests::steps_that_supersede_a_seek_before_its_worker_starts_it_land`
    holds the cache's lock across that seek and the steps, on a thread
    that panics if it blocks, so the seek's playhead can't reach the
    cache and the steps supersede the seek before its worker starts it;
    without the step setting the playhead it stuck on frame 0 every
    time;
  - two instant steps before a present go 2 frames, with no request to
    the worker, and a step of 1 and one back come back;
  - with an A/V offset of ±50 ms, a paused exact seek to 10 shows 10
    over 100 presents, with the clock at 10, and playing, the time
    picked is the clock's plus the offset;
  - two streams, as the X5's lenses: on three slots a seek and steps of
    1, 5 and −1 land on both (`two_streams_on_three_slots_seek_and_step`),
    and on four with stream 1's decoder 500 ms a frame and 300 ms a
    seek, seeks to 40, 10 and 70 do
    (`two_streams_seek_with_the_second_decoder_behind`). Both failed
    without the upload thread's share of one while waiting; the first,
    and `player::tests::two_streams_land_with_a_slot_dead` (four slots,
    one Dead), without the presenter showing each stream's as it comes.
    On three slots with the decoder opened first failing its seek while
    its next frame holds the third slot, the other's frame for the seek
    lands (`two_streams_on_three_slots_seek_with_one_failing`); it
    stuck when the failed stream kept its slots, the re-review's
    mutation M3, which every test passed before. On three slots with
    the cache full (16 frames, 5 ms a frame), keyframe seeks back to 45,
    29, 21, 13 and 5 show 40, 24, 16, 8 and 0 on both streams
    (`two_streams_keyframe_seek_back_on_three_slots_with_the_cache_full`,
    and `player::tests::two_streams_keyframe_seek_back_with_a_slot_dead_and_the_cache_full`
    on four slots, one Dead). With a landing unpinned once in a slot,
    both stuck at [40, 60] (the re-review's finding). Playing on three
    slots with the second decoder 150 ms a frame, frames 1 to 12 show on
    both (`two_streams_play_on_three_slots_with_the_second_decoder_behind`);
    without the share of one while a frame at the playhead waits for a
    slot it stuck at [1, 0] in 16 of 16 runs;
  - a keyframe seek back with the cache full lands on its keyframe: 8
    frames of budget, played to 60, then keyframe seeks to 45, 37, 29,
    21 and 5 show 40, 32, 24, 16 and 0, with the clock there. It failed
    without the pins;
  - a fault injected at frame 10 of a VA-API decode (`FaultBackend`,
    with media's `fault-injection` turned on for playback's tests) falls
    back to software. Every frame from 0 to 20 is shown in turn, none is
    dropped, the snapshot shows the fallback and the decode path, and
    one `FellBack` event comes;
  - `Auto`, with doctor's checks saying that Vulkan decodes H.264 here
    and VA-API doesn't, picks Vulkan, which falls back on lavapipe, and
    the snapshot and the events say so, with the fallback's reason. The
    backend hands every Vulkan decode FFmpeg's device on lavapipe, by
    name, so that a machine whose GPU decodes H.264 with Vulkan Video
    (odin, where hw-test runs Tier 1) falls back too (D38: a render
    node that matches no GPU still takes the first GPU listed);
  - with an `AudioOutputFactory`, video follows the audio's clock, and
    after a seek it waits for the audio's preroll, Seeking, before the
    clock runs from the seek's time; seeks made while the audio opens
    leave its open alone;
  - `set_config` reaches the upload thread (the budget, in the cache and
    the snapshot) and the slots;
  - risk 11's acceptance: 60 frames of 5.7K (5760 × 2880, 24 MB each)
    through a 256 MiB cache and four slots, frame by frame. Every frame
    is shown in turn and none dropped. The cache peaked at 237 MiB and
    never passed its budget, with no "too small" notice and no slot
    dropped. It took 7.7 s; the upload's CPU time on lavapipe was
    125 ms at the median and 150 ms at the most.
- `crates/playback/tests/never_blocks.rs` (plan section 8.6), on a
  thread marked with `OnBlock::Panic`:
  - **gated**: a backend gated forever, and 3 s of a 60 Hz UI loop with
    a seek every fifth iteration and play and pause toggled. Iterations
    took 96 µs at the median, 179 µs at the 99th percentile and 248 µs
    at the most. The state was only Opening or Seeking. After the
    release the first frame came up in 1.3 ms;
  - **the storm**: 1000 seeks in 2 s, on a decoder that takes 50 ms a
    frame. Iterations took 128 µs at the median, 199 µs at the 99th
    percentile and 264 µs at the most. The last seek's frame came up.
    One of the 1001 requests decoded to completion and 1000 were
    superseded, and the player was Paused at the end. At most 2 plus
    the gaps of a frame's decode (50 ms) between seeks may decode to
    completion: the open's first frame, the last seek, and a seek that
    nothing superseded for a frame's decode, as a preempted UI thread
    allows. The bound was "fewer than 100", which a worker ignoring its
    stop token passed with 8; it fails the bound now, with no gap;
  - **shutdown**: a player stuck at the gate joins cleanly, in 12 to
    160 ms over the runs, its upload, open and decode threads all
    joined, within the plan's 2 s. A player dropped while stuck closes
    its cache, the gate stays shut, and its open and decode threads
    end: the last holder of the backend lets go. With `Player`'s `Drop`
    taken out, the open waited at the gate for good, which the test
    checked nothing of before;
  - **the negative control**, `a_blocking_snapshot_on_the_ui_thread_panics`:
    `snapshot_blocking` on the marked thread panics with "taking the
    player's status lock would block the UI thread", and a slot's
    `gpu_frame` panics too, each naming the test's line.
  - With 4 busy loops on the 4 CPUs, both binaries passed five runs in
    a row, the engine's in about 20 s.
  - Every engine and `never_blocks` test but the stuck one ends with
    `common::join`: 10 s for the threads (`JOIN`), failing with the
    `ShutdownReport`. Under a build and 4 busy loops, `join(2 s)`
    failed 1 to 3 tests a run, every one with the upload thread still
    waiting for lavapipe, and printed nothing.
- `playback::slots`: a slot goes round its states and is claimed only
  when the ring alone holds it; a view of a slot that moved on is
  refused; a slot without memory is out for good; slots past a smaller
  setting give their memory back.
- `playback::decoder`: `Auto` picks the kind doctor found working, for
  each pair of results and on a discrete GPU or not; it decodes in
  software what no check covers; set preferences are kept.
  `doctor::check_format` maps streams to the checks that cover them.
- `frames_between` counts by run places, else by time.
- `crates/playback/tests/cache.rs` gains `an_offer_whose_token_stopped_puts_nothing_in`,
  and its negative control covers `try_insert_until`. The cache's
  proptest passes again at `PROPTEST_CASES=100000`, in 27 s of a release
  build.
- Three faults found while the engine was written each have a test that
  fails without its fix: a worker of a file closed meanwhile could put
  a frame into the next file's cache, between the next open's clear and
  its own stop (`an_offer_whose_token_stopped_puts_nothing_in`); a seek
  made while the audio opened stopped the audio's open, since both took
  the requests' token (`a_seek_while_the_audio_opens_keeps_the_audio`,
  now that opens have their own `StopSource`); and the presenter put
  the playhead back to a step's start once, under the worker that had
  moved it on, which left the step waiting for room for good
  (`a_step_past_the_window_ahead_lands`). A fourth, a keyframe seek
  setting the clock to the time asked for rather than the keyframe's,
  has `a_keyframe_seek_shows_the_keyframe_at_its_own_time`. A fifth, the
  upload thread keeping an explicit size whatever the memory, against
  `budget::recheck`'s rule above, has
  `player::tests::a_set_budget_shrinks_too_when_memory_is_short`, on a
  reading of 512 MiB available.
- The engine's review found 19 faults, all fixed or corrected in the
  docs, each with a test that fails without its fix where one can show
  it (the tests above): two streams deadlocked on GPU slots after a
  seek or step whenever fewer than two slots a stream were live, or
  the second decoder came late; a keyframe seek back with the cache
  full never landed; two Tier 1 tests took odin's Vulkan Video for none
  (D37); the seek storm's bound couldn't fail; a step on its way lost a
  seek, and an instant step didn't move the next one's base; the cgroup
  cap missed a namespaced container's limit; a recheck counted the
  cache against itself; xtask's machine tests raced on one fixture
  directory (D37); the tests' 2 s joins failed under load and printed
  nothing; the dropped-player test passed without `Drop`; a block on
  the UI thread named the helper, not its caller; frames alive past the
  budget went unsaid; a paused frame moved with the A/V offset, whose
  doc had the sign backwards; hitches were judged against the capped
  latency; a doc comment was on the wrong function; and the catch-up
  count was stated as fixed.
- The benchmarks' harness runs in Tier 1
  (`the_bench_harness_plays_on_lavapipe`): a second of 30 fps H.264
  through both passes on lavapipe in software, on a marked thread. All
  29 frames after the first came up unthrottled; the real-time pass
  counted its 2 seconds, looped twice in 2.5 s and gathered the upload,
  render and UI stages. It checks what the harness counts, not how
  fast, and passed three runs in a row beside 4 busy loops. Run once by
  hand on lavapipe with doctor's checks faked to say Vulkan decodes
  H.264, the gated path recorded every row and note, and the gate
  failed as it should, naming the fallback to software and the drops
  by second.
- On this box (no GPU), with `SPHERECUT_TIER2` set and the X5 samples,
  each benchmark skips with its reason: no perf clips (hw-test makes
  them, except with `--quick`) or a perf clip not found, no stitched
  equirect among the samples, and "no GPU: the adapter
  SPHERECUT_ADAPTER=auto picks is a CPU, llvmpipe".
- The third part's first `cargo xtask check` failed
  `frames_passed_over_while_playing_count_as_dropped`. Of 18 more runs
  of the engine binary, two failed it again, one of them
  `playback_after_a_seek_waits_for_the_audio_preroll` too; 40 runs of
  the first test alone passed. In the
  first, the presenter's playhead for frame 8 had found the cache's
  lock taken, and the test waited 20 s for frame 10 to come up in a
  slot without presenting again: the cache kept the playhead at 6, so
  the upload thread wanted 6 to 9, never 10. In the second, a snapshot
  that found the status lock taken was the copy from before, the empty
  default, with `has_audio` false (`fresh` says so). The tests now
  present while they wait and read fresh snapshots (`common::snapshot`,
  which the benchmarks' stream rate and decode paths use too), and the
  presenter asks for a repaint when the cache's lock loses its playhead
  (`a_playhead_the_cache_was_too_busy_for_asks_for_a_repaint`, which
  holds the lock over a present on a marked thread and fails without
  it). Then 20 runs of the engine binary passed, and 5 of each
  playback binary beside 4 busy loops.

Revisit if: H1's bench or the 256 MiB-budget 5.7K run shows decode
waiting on the cache while the window has room, or the window by time
starving a stream with large frames; a cgroup limit that sits on an
ancestor isn't seen in a Flatpak; or the overlay needs samples that an
atomic ring can't give (more than one value per sample).

For the engine: H1's real-time benches show the presenter dropping
frames with decode and upload keeping up, or catch-up starting too late
or too often; a GUI's paint callback holding slots so long that the
ring runs dry; the present latency of one UI frame being visibly off
the vsync's on the GUI (slice 7); or `never_blocks` failing its 10 ms
ceiling on a machine that isn't a VM.

## D35. Audio and A/V sync

*2026-10-30, M1 slice 6. The M1 plan's D33 (section 9), adjusted to
what was built: the file's audio decoded onto the clip's time axis, an
output crate whose clock is the sound card's, and the engine's video
following that clock. The GUI's notices and the overlay's numbers are
slice 7's and 8's; the device itself is tested in H2.*

What:

- **Decoding** (`media::audio`, `FfmpegAudio`, through
  `MediaBackend::open_audio`).
  - Its own confined demuxer and decoder (D24), opened by the codec id
    as an int, with the sample format and channel layout read as ints
    (D11). The first audio stream with a decoder, unless the request
    names one; a file with none is `MediaError::NoStream`, code
    `no_stream` in the CLI contract.
  - Out comes packed f32 at the output's rate and channel count, in
    chunks placed on the clip's axis: output sample `s` is clip time
    `s / rate`, and clip time 0 is the first video stream's first
    presented frame (D29), which the request gives as `origin` (`None`
    puts 0 at the audio's first sample). Samples before 0 are dropped,
    and audio that starts after 0, or after a seek's time, is led by
    silence, so chunks follow each other without a gap.
  - libavcodec removes AAC's priming and the demuxer applies edit lists.
    The last frame's padding (AAC codes whole frames of 1024) is cut
    where the packets' durations say the stream ends.
  - Channels: mono is duplicated and stereo passes through. First-order
    ambiX (`SA3D`, D17) is read as four channels in no order, so that
    swresample can't remix it as 4.0 surround, and decoded here to two
    virtual cardioids at ±90°: L = ½(W + Y), R = ½(W − Y) (ACN, SN3D).
    Anything else is swresample's default downmix to stereo. Stereo then
    goes to the output's channels: averaged for one, into the first two
    of more, with silence in the rest.
  - **The resampling grid.** The resampler only ever starts at an input
    sample a whole number of periods from the stream's first, the period
    being the input rate over the two rates' greatest common divisor
    (147 samples for 44.1 to 48 kHz). There its phase is the one a decode
    from the start has, so a seek's output after its preroll is a
    sequential decode's.
  - **Seeking** goes back to 100 ms before the time (at least two AAC
    frames, for the MDCT's overlap), flushes and decodes; the resampler
    starts at the first grid point in the first frame decoded, and what
    comes before the exact sample is dropped: the next chunk starts at
    sample round(at × rate).
  - **Coarse time bases.** Matroska's are milliseconds, coarser than a
    sample, so each frame's time is rounded. A frame within a tick of
    where the frames before it end is placed there, from the stream's
    first sample, so a seek lands each frame where a decode from the
    start puts it. Each packet's end is rounded too, so the last frame's
    padding is cut only from a frame that goes past its packet's end by
    more than a tick.
  - 50 packets in a row refused make the stream undecodable.
- **The output crate** (`spherecut-audio`).
  - cpal 0.18.2 with only its `pulseaudio` feature: the PulseAudio
    protocol host, a pure-Rust client that reaches PipeWire through
    `pipewire-pulse`, and ALSA, which cpal always builds on Linux.
    `alsa-sys` links libasound, so the ALSA headers and pkg-config are a
    build prerequisite: `cargo xtask check` stops with the install
    command when pkg-config finds no `alsa`, and hw-test's prerequisites
    say the headers block the build. A `[[bans.features]]` allow list in
    `deny.toml` keeps cpal to `pulseaudio` (D21).
  - **The device** (`Output::open_device`, on the `audio-out` thread):
    cpal's default host (PulseAudio when a server answers, else ALSA),
    its default output device, at its default rate and channel count.
    f32 when the device takes it at that rate and channels, else its
    default format, else i32, i16 or i24, converted from f32 in the
    callback with cpal's `FromSample` through a scratch buffer allocated
    when the stream is built. The buffer asked for is
    `BufferSize::Fixed(1024)` (21.3 ms at 48 kHz) clamped to the
    device's range, `Default` when the range is unknown, and `Default`
    again if the fixed size is refused.
  - **The latency** is cpal's `playback − callback` from the callback's
    timestamp, plus the frames already rendered when a buffer is
    rendered in pieces.
  - **The chunk ring** is the plan's: SPSC, 512-frame chunks, about
    500 ms, samples as `AtomicU32` f32 bits, atomic chunk headers (epoch,
    first sample, frames), head and tail Release and Acquire, and a
    decode thread that sleeps 5 ms while it is full. The consumer asks
    for samples by their index (placement), and while the ring is full
    the producer may reclaim the oldest chunk by compare-and-swap if it
    is of an older epoch, since a paused stream's callback may not run
    to drop it. The consumer reads a chunk's header and then its samples
    with the tail checked after each (Acquire fences), and the producer
    writes a slot after a Release fence, so a chunk reclaimed while it
    is read is read again rather than taken as a gap.
  - **The clock** (`AudioClock`, a `Clock`). Its state is one
    `SeqLock<12>` (core's, now public): epoch, base and seek times,
    running, on the system clock, on audio, and the anchor's wall
    instant, time heard, limit and floor.
    - A seek (`set`) and a pause each start an epoch: the decoder seeks
      to its time and the callback drops every other epoch's chunks.
    - Paused, it shows the base time. Playing, it waits at the base
      until the epoch's first audio plays. Then each callback anchors
      it: at the callback's instant the time heard is the buffer's first
      sample less the latency, and the clock runs on from there for at
      most the latency plus two buffers (the end of the buffer written
      plus one more), so a stalled callback freezes it. It never shows
      less than it showed when the anchor was made, so jitter in the
      callback's timing holds it instead of moving it back. That holds
      for a time read at the instant it is read; the presenter reads it a
      present's latency ahead, so it keeps its own hold (below).
    - **The system clock.** When the device is lost, or the callbacks
      stop for 500 ms while playing (`STALL`, a suspended sink), or no
      audio arrives for 2 s after playing starts (`WAIT_LIMIT`), the
      clock runs on the monotonic clock from the time it showed, and
      asks the decoder for audio 300 ms ahead (`RESYNC_LEAD`). A
      callback that finds the clock there plays the sample due when its
      buffer is heard, if the ring has it, and anchors the clock to the
      audio again without a jump; if the audio is more than twice the
      lead late, it asks again from the clock's time.
  - **The callback** (`render`) is the same code for all three outputs.
    It takes no lock, allocates nothing and logs nothing; its numbers
    go to atomics that `audio-out` and `stats()` read. When the ring
    runs dry while playing it plays silence and the clock runs on: the
    next buffer asks for the sample due then, so the samples that
    weren't there are skipped (the plan's underrun debt). A dry spell
    counts as one underrun, and none past the end of the audio.
  - **`audio-out`** keeps the stream, plays it when the clock runs and
    pauses it 200 ms after the clock pauses (so the device's queue is
    silence by then; a stream that can't pause plays silence), runs the
    watchdog every 50 ms, and reads the stream's errors. `Xrun` is
    counted. `DeviceChanged` (cpal 0.18 rerouted the stream to a new
    default device, which goes on) and `RealtimeDenied` are logged. Any
    other error loses the device: the clock goes to the system clock and
    the default device is tried again every 2 s at the same rate and
    channel count, which the decoder already resamples to. A stall that
    lasts 2 s on the system clock drops the stream and opens it again,
    and a new stream gets 2 s to call back before it is opened again.
  - **No device**: `Output::null` runs the callback from an
    `audio-null` thread every 1024 frames on a schedule kept from its
    start, at the schedule's instants (not when the thread wakes), with
    no latency, starting the schedule again when it is 8 periods behind
    (a suspended machine). The engine runs the same code on it.
  - **`FakeAudioDevice`**, for the Tier 1 A/V tests: callbacks at
    scripted instants (a period, a stated latency, stalls, loss and a
    new stream), the real callback and the watchdog every 50 ms of
    scripted time, and a record of each callback (instant, when heard,
    epoch and first sample, fill, first sample value). Scripted time
    runs ahead of real time, so before each callback the fake waits, in
    real time, for the audio that callback will play.
- **The engine** (`playback::audio`).
  - `DeviceAudio` is the `AudioOutputFactory`. On the open worker it
    opens the file's audio once to see that there is a stream the
    backend can decode (`NoStream`: the file has no audio, the event
    `NoAudio`, and video runs on its own clock); then the output, the
    default device or else the null output at 48 kHz stereo with a
    warning, or one given beforehand (`AudioDevice`); then the
    `audio-decode` thread.
  - `audio-decode` waits for `AudioOutput::start(origin)`, which stream
    0's decode worker sends when it opens, then opens the decoder at the
    output's rate and channels and decodes each epoch the clock starts
    into the ring. A seek or decode error ends the epoch there, so the
    clock runs on silence; a decoder that won't open leaves every epoch
    ending where it starts.
  - **The threads are the player's.** The factory gets the player's
    `PlayerThreads`, which starts `audio-decode` and adopts the output's
    thread, so that `Player::join` waits for both within its budget and
    reports them. `audio-decode` ends when the open's token stops (a
    newer open, or the player's shutdown, whose flag interrupts its
    reads and the probe's), and stops the output's thread as it goes
    (`Output::stop`, which hands the thread over without waiting).
    Dropping the output stops both without waiting, so it may be
    dropped anywhere, the UI thread included; a newer open drops the
    last file's outside the status lock, and pauses its clock at once.
  - The player follows the output's clock once the open is done
    (playing it at once if the player's clock was playing), and emits
    `AudioOpened` with the output's numbers. A preroll ends when the
    video target is ready and 100 ms of audio is queued, or after
    `PREROLL_LIMIT` (2 s) without the audio, when the clock falls back
    to the system clock and the player emits `AudioLate`.
    `Player::audio_stats()` reads the output's numbers without waiting.
    The presenter reads the clock a present's latency ahead (D34); while
    playing it never takes a time less than the last present's unless
    the clock went back at its own instant (a seek), since a late
    callback's correction can bring a time read ahead below the one
    read before.
    `play_at`, `pause_at` and `seek_at` take the instant, for tests on
    scripted time.
- **Test clips.** synth's audio clips are AAC without perceptual noise
  substitution (`aac_pns=0`): FFmpeg's decoder fills those bands from a
  random generator that a seek restarts, so a clip that used it would
  decode differently after a seek, at its noise floor.

Where this differs from the plan:

- The underrun debt is placement: the callback asks the ring for the
  sample due instead of counting the samples owed. The effect is the
  plan's, and nothing has to be kept or reset across a seek.
- A pause starts an epoch at the paused time, where the plan froze the
  clock and played silence. Without it the samples queued in the device
  when it paused would not be heard, and playing again would go on from
  after them, a latency's worth ahead of the clock.
- `DeviceChanged` doesn't lose the device: in cpal 0.18 it says the
  stream was rerouted and goes on.
- The clock goes back to the audio from the system clock when callbacks
  come again, without a jump; the plan stopped at the system clock.
- `WAIT_LIMIT` and `PREROLL_LIMIT` are new: a device that takes a stream
  but never calls back, or audio that never comes, would otherwise hold
  video still.
- testkit's `require_audio_device` takes the function that opens the
  device, since testkit has no dependencies.
- The Tier 2 test checks the median latency, not each callback's, and
  up to 400 ms, not 200: on odin it passed at 182 ms on Bluetooth (A2DP,
  the Edifiers Pat listens on) and 64 ms on HDMI, with the clock within
  0.01 and 0.14 ms over 2 s and no xruns, and a weaker link or another
  codec would go past 200, while A/V sync follows whatever latency the
  host reports.

Why: the plan's (section 9.3): the DAC's clock is the master, so video
repeats or drops frames to follow it and there is no drift to correct.
One callback for the device, the null output and the fake device means
the Tier 1 A/V tests run the code that plays. The PulseAudio protocol
host needs no C library and is what Fedora and Arch serve through
PipeWire; cpal's `pipewire` and `jack` hosts link libraries no build
machine should need (libpipewire's development files and libclang, for
`pipewire`). The stream lives on `audio-out`, since building or dropping
one can block on the sound server, and epochs flush the ring without a
lock in the real-time callback.

Tested:

- `synth/tests/audio.rs`, 13 tests on clips synth writes: exact sample
  counts, the tones' pitches, priming gone and the audio on the video's
  axis, seeks equal to a sequential decode within 1e-6 at 48 kHz and
  44.1 resampled, in MP4 and in a Matroska remux, 44.1 kHz resampling
  to exactly 10 s at 48, mono, 5.1, ambiX, the output's channels,
  origins before and after the audio, `NoStream`, a Matroska remux
  with every sample of the MP4's from its first, at one lag within a
  millisecond, at 48 and 44.1 kHz, and `sync-flash-beep`'s beeps within
  1 ms of their white frames.
- The audio crate: the ring's unit tests (placement, gaps and other
  epochs, reclaim, a two-thread stress test, and a reclaim racing an
  epoch's first fill), the clock's (rounding, packing, the floor and the
  limit), `audio-out`'s stall rule (a stalled stream opened again once
  each 2 s), and 13 tests in `tests/clock.rs`,
  most on the fake device: the clock waiting for audio and then running
  by the samples played; never going back, and freezing when callbacks
  stop; an underrun skipping its debt, and the end of the audio not
  counting as one; a seek to round(at × rate), and one past the end;
  a pause and play resuming at its sample; `ready`; loss and a stall
  falling back without a jump and the audio taking over again; the wait
  limit; the null output, its callbacks a period apart; and opening the
  device here.
- `playback/tests/av_sync.rs`, 10 tests through the whole engine, most
  on the fake device (the synthetic backend's audio, the real decode thread
  and presenter): 60 s of scripted time at each of 29.97 and 59.94 fps
  with 44.1 and 48 kHz, the shown frame at every present the one whose
  interval holds the clock's time; a seek restarting audio at
  round(at × rate); a 200 ms decode stall leaving the clock running,
  video continuous and the sample heard after it equal to the clock;
  device loss without a jump over 1 ms; 20 callbacks 30 ms late without
  the time presented going back; a preroll that waits its limit; the
  null output within ±5% over 2 s; `AudioDevice::Default` on the null
  output when no device opens, as here; a file without audio; and the
  audio's threads ending with a newer open and joined, a stuck decoder
  included, by the player's join. A unit test drops an output on a
  marked UI thread and gets both its threads handed over.
- The slice's review found 8 faults, all fixed, each with a test that
  fails without its fix but the last two: a stalled stream was opened again
  every 50 ms instead of every 2 s; a chunk reclaimed while the callback
  read it could be taken as a gap; the time presented could go back up
  to 9 ms after a late callback; Matroska's end cut took 16 samples off
  each of the first frames, and no test caught a broken `place()`; the
  audio's threads weren't the player's (not stopped by its shutdown,
  not in its join or report, joined inside the status lock); the null
  output anchored at its wakeups; the probe's interrupt flag was never
  set; and this entry gave the seek's grid point the wrong way round.
- `audio_output_device_plays` (Tier 2): a 440 Hz tone on the default
  device for 2 s, passing when the clock advances 2.0 s ± 50 ms and is
  monotonic, no callback comes over twice its buffer after the last,
  the median latency is 5 to 400 ms, nothing underran and the clock
  stayed on the device; it notes the host, device, format, buffer and
  latency, and records the numbers. Here it skips with ALSA's "The
  requested audio device is not available". Run once by hand on the
  null output, it measured 1.99992 to 1.99999 s over 2 s and a worst gap
  of 1.009 buffers in three runs.

Limits:

- A file whose AAC used perceptual noise substitution decodes those
  bands differently after a seek than from the start, at their noise
  floor; the 1e-6 seek equality is shown on clips without it.
- Matroska audio is placed within a millisecond of where the MP4 puts
  it, and, with no count of AAC's padding, may end up to a frame later;
  a frame within a tick of its packet's end keeps that much padding.
- The output's format is fixed when it opens. A new default device that
  can't play that rate and channel count isn't used, and the clock stays
  on the system clock, silent, until one can or the file is opened
  again.
- The latency is what the host reports; nothing here measures sound
  against a microphone. The A/V offset setting (slice 8) covers the
  rest, and H2's `gui_smoke` measures |A/V| on the sync clip.
- On this box (no audio device) the cpal stream, `audio-out`'s loop,
  the error handling and the retry are only built, not run. On odin the
  device test has run (at 855bb54, PulseAudio's protocol to PipeWire,
  f32 at 48 kHz, 1024-frame buffers, on Bluetooth and on HDMI); losing
  the device, the retry and the stall rule have still only run on the
  fake device.
- Ambisonic audio is a basic stereo decode, not steered by the view
  (M6 adds binaural).

Revisit if: H2's `audio_output_device_plays` fails on a machine's
PulseAudio or PipeWire, or `gui_smoke`'s |A/V| p95 reaches 20 ms on the
sync clip; a camera's AAC uses noise substitution and its seeks sound
different; someone needs to choose the output device; or cpal changes
what `DeviceChanged` means.

## D36. The GUI's dependencies, features and Wayland gaps

*2026-10-30, M1 slice 7. The M1 plan's D34 (sections 7 and 11), as
built. Updates D21 and D22, and records Pat's answer on the fonts.*

What:

- **The crates** (D7): eframe, egui and egui-wgpu `=0.37.0`, egui_tiles
  `=0.18.0`, winit 0.30.13, and egui_kittest `=0.37.0` for the tests
  (D22's rows). eframe has no default features: `wgpu_no_default_features`
  (Vulkan only, as render's wgpu), `wayland`, `x11`, `default_fonts`,
  `persistence` (the layout and settings) and `clipboard` (the notices'
  Copy). winit is named to turn `wayland-dlopen` back on, which
  eframe without its defaults drops, so libwayland is loaded at run time
  as libxkbcommon is; from slice 8 the smoke run also reads the
  monitor's refresh rate through it. image (PNG only, D22) is the app's
  from slice 8 too, for the smoke run's screenshot.
- **Not used:** eframe's `accesskit` (zbus 5 and RUSTSEC-2026-0370 to
  -0372; Pat's M1 answer is no screen reader until accesskit is on a
  maintained zbus), `links` (it opens a browser), `system_fonts` and
  `glow`; winit's `wayland-csd-adwaita` (RUSTSEC-2026-0192 through
  ttf-parser); `rfd` (zbus through its portal, or GTK 3), so the Files
  pane stands in for a file dialog. egui still depends on the `accesskit`
  crate itself, the tree's data types, which bring no zbus.
- **deny.toml.** `[[bans.features]]` entries keep eframe's `accesskit`
  and `links`, winit's `wayland-csd-adwaita` and egui_kittest's `eframe`
  (which turns eframe's accesskit on) from coming back (D21). The graph
  is Linux's only (`[graph] targets`, x86_64 and aarch64): arboard, the
  clipboard, turns on image's `bmp` and `tiff` for its Windows and macOS
  clipboards, which Linux never builds, and image's PNG-only allow list
  (D22) holds for Linux. Advisories pass with no ignores.
- **Fonts.** egui's default fonts (Ubuntu-Light and Hack in
  `epaint_default_fonts`) are OFL-1.1 and Ubuntu-font-1.0. *Pat decided
  on 2026-10-24:* keep them, with a license exception in `deny.toml` for
  that crate only. Both licences allow the fonts to be embedded in a
  GPL program. They have no emoji or arrows: a missing glyph is drawn
  as a box (and fails a kittest), so the UI's buttons and the keys' help
  are words ("Play", "Prev", "Dismiss").
- **The device.** eframe takes render's own device through
  `WgpuSetup::Existing` (D31), made by `Gpu::for_window` before the
  window, with `multisampling: 0` and `depth_buffer: 0` so that egui's
  pass has one sample and no depth, as the viewer's pipelines do.
- **The viewer's callback** (plan section 7.3, adjusted):
  - The rect is rounded inward to whole physical pixels and inset by one
    pixel on each side (`viewer::pixel_rect`). At 1.25 pixels a point the
    tiles' separator, antialiased, drew over the viewer's last column;
    inside the inset, every pixel is the projection's alone.
  - Whether to project again is a `DrawKey` (the slot, the frame in it,
    the view and the filter) that `prepare` compares with the last one
    projected, not a `redraw` flag set in `ui()`: a flag cleared by a
    pass that was never rendered would have lost its redraw. A resize
    projects again too.
  - A projection counts as drawn only once `paint` ran after it (an
    atomic flag in `ViewerResources`). egui-wgpu records `prepare`'s
    commands before it acquires the window's surface, and drops them
    when the acquire fails (Outdated, Lost, Timeout, Occluded); the next
    pass, with the same key, would otherwise only encode a target that
    was never drawn, or was drawn at the old size.
  - `prepare` records with `Projector::record_unchecked` and makes the
    target with `LinearTarget::new_unchecked`: no `Gpu::check`, which
    polls the device on the UI thread and takes the shared error slot
    that the upload thread watches (D31). The upload thread reports a
    lost device (`GpuFailed`); both filters' pipelines are built with
    the resources, before the first frame.
  - `SpherecutApp::logic` takes the player's events, the doctor's
    result and the jobs, and picks the frames to show (`present`).
    eframe calls it before each `ui`, and alone while the window is
    minimized or occluded, when it runs no egui pass: the presenter
    moves the cache's playhead and ends the clip, so playback goes on
    hidden. `ui` runs `logic` itself when nothing ran it first, so
    kittest, which calls only `ui`, runs all of it.
  - Jobs keep their threads' handles. One given up (a listing for a
    folder left since, a probe for a file closed since, any at exit) is
    kept in `jobs::Threads`, and `run` joins them after the event loop
    with 3 s, beside the player's 2 s, so that none is inside a driver
    or FFmpeg while the process's exit tears those down. One still
    running then is left, and logged.
  - Notices of the machine (the doctor's, the CPU adapter's, a failed
    GPU, no audio device, a slot the GPU dropped) outlive a file, and a
    dismissed one stays dismissed when the next file raises it again.
    `open` takes the last file's pending events before it forgets that
    file's notices. A worker that checked its open was current just
    before the next open can still send one event after it, which then
    lands on the next file: the events carry no open (a playback
    change, if it ever shows).
- **The UI thread never blocks** (D34). `crates/app/clippy.toml`
  repeats the workspace's (a crate's file replaces it) and bans std's
  and egui's (epaint's) lock, read and write, the condvar waits,
  `Receiver::recv`, `JoinHandle::join`, `thread::sleep`, the blocking
  `fs` reads, `wgpu::Device::poll`, `pollster::block_on` and the
  player's `join`. The startup write into egui-wgpu's callback
  resources, the directory listing on its worker, the joins after the
  event loop and the tests' pacing say why with `#[expect]`. Jobs on
  workers (the doctor, the listing, the Info pane's probe) come back
  through a channel the UI only `try_recv`s.
- **Recorded limits of winit 0.30.13:** no touchpad pinch on Wayland or
  X11 (two-finger scroll and Ctrl+scroll zoom instead), and no file drop
  on Wayland (the command line and the Files pane open files). GNOME
  draws sctk's plain frame, as `wayland-csd-adwaita` stays off. Without
  `WAYLAND_DISPLAY`, `WAYLAND_SOCKET` or `DISPLAY` (winit's own rule)
  the GUI exits 3 with `no_display` (D27).

Why: this graph passes advisories with no ignores. accesskit brings
zbus's three unmaintained-crate advisories, and csd-adwaita brings
ttf-parser's. The cost is no screen reader in M1 and a plain GNOME
frame, both revisited when upstream moves. Bundled fonts were a licence
question, which the prompt makes Pat's. Comparing what was drawn,
rather than remembering a flag, can't lose a frame however eframe
schedules its passes.

Tested: the kittest suite on lavapipe (`crates/app/tests`, 16 tests):
the viewer is `render_still` byte for byte at 1, 1.25, 1.5 and 2 pixels
a point; an unchanged frame and view only encode, and a resize projects
once; a projection never painted is projected again, by `prepare` and
`paint` called as egui-wgpu calls them; `logic` alone presents frame
after frame and ends the clip, which nothing ends without it; a resize
sweep from 1×1 to 3840×2160 never panics; GUI `never_blocks` (D34); the
panes and notices, a dismissed machine notice kept across an open. Unit
tests: a job given up leaves its thread until it ends, and the exit's
join takes the threads that end by its deadline and names the rest. `cargo deny check` passes
with the font exception the only licence exception added.

Revisit if: accesskit moves to a maintained zbus (screen readers),
winit gains Wayland pointer gestures or file drop, egui_dock is
published for this egui (floating panels, D7), or GNOME's frame looks
wrong in H2's screenshots.

## D37. Hardware testing in M1

*2026-10-29, M1 slice 5, from the M1 plan's D35 (section 15); finished
2026-10-30 in slice 8. Slice 5 landed the headless part, for H1; slice 8
added the GUI smoke run and its session discovery, for H2. Updates D19
and D20.*

What:

- **Tier 1 on the hardware machine.** After the Tier 2 tests,
  `cargo xtask hw-test` runs `cargo test --workspace --all-targets` in
  the debug build, as `cargo xtask check` does, and reports every
  failure with its output in "Tier 1 on this machine". A failing test,
  a Tier 1 test that skips, a binary that dies, or a run that fails with
  no test binary to blame fails hw-test. `--no-tier1` leaves it out.
  Tier 1's tests of a machine without Vulkan Video see only lavapipe
  there: frame's `--decoder vulkan` row runs with the Vulkan loader
  shown lavapipe's ICD alone, and the engine's `Auto` test hands FFmpeg
  lavapipe's device by name. On odin they took the RX 7600, which
  decodes H.264, and would have failed every run; a render node that
  matches no GPU wouldn't do, since the first GPU listed is taken then
  (D38). xtask's own machine tests give each fixture tree a directory
  of its own, since two that shared one raced under libtest.
- **Prerequisites** gain rustc 1.95 or newer (it blocks the build), a
  lavapipe ICD (`mesa-vulkan-drivers` on Fedora, `vulkan-swrast` on
  Arch; the Tier 1 render and playback tests need it) and the ALSA
  headers (`alsa-lib-devel`, `alsa-lib`; audio from slice 6, not the
  build yet). strace was already one (D24).
- **The machine, read only** (`xtask/src/machine.rs`):
  - "Power profile": power-profiles-daemon's profile
    (`powerprofilesctl get`), ACPI's `platform_profile` and its
    choices, cpu0's frequency driver, governor and energy preference,
    and each amdgpu card's `power_dpm_force_performance_level` and the
    active mode of `pp_power_profile_mode`. Each says "not available"
    and why when it can't be read. Read once, before the build.
  - "Render nodes": every `renderD<n>` with its PCI slot, vendor,
    device, driver, and whether it opens for reading and writing.
- **The perf clips** (`xtask/src/perf_clips.rs`, synth's
  `media::perf_clips`): synth's moving grid with seeded noise on every
  frame, 20 s at 30 fps, coded by libx265 in open GOPs of 30 with three
  B-frames at an average bitrate (one-pass ABR, synth's
  `Spec::bitrate_kbps`):
  - `hevc8-5k7-30fps-noisy-20s.mp4` and `hevc10-5k7-30fps-noisy-20s.mp4`,
    5760×2880 at 100 Mbps;
  - `hevc8-8k-30fps-noisy-20s.mp4`, 7680×3840 at 120 Mbps (the X5's 8K30
    is two 3840² streams at about 125 Mbps in all).
  - They are made once into `<target>/hw-media/` and kept across runs.
    Each carries the spec it was made from in `<file>.spec`, and is made
    again when synth's spec changes. A clip is written in `.partial/`
    and moved into place whole. The Tier 2 tests find them through
    `SPHERECUT_PERF_MEDIA` (`tier2::require_perf_clip`, which skips while
    it is unset, as with `--quick`, or a clip is missing).
  - "Perf clips" gives each clip's size, bitrate, whether it was cached
    or made (and in how long), and its software decode rate from
    `bench_perf_clips_software_decode`, flagged **too easy** above
    150 fps. A clip that can't be made fails hw-test, with why.
- **The pass rules** for "5.7K30 stitched equirect plays in real time"
  (plan section 12.3) are the playback benchmarks' gate (D34): on the
  hardware kind `Auto` picks, no drop and no decode-drop after the first
  second of 60 s, and every unthrottled second decoding at least
  1.2 × 29.97 frames. A skip is never a pass.
- **Benchmarks** (D20): the playback benchmarks record one run's seconds
  as one row, named "… per second", whose runs are the seconds and whose
  range is the slowest and the fastest second. The report says so.
- **`--list`** prints the Tier 2 tests, as the report names them, from a
  debug build, and stops.
- **Requests** are batched: H1 (headless, after slice 5, from any
  terminal), H2 (the GUI, after slice 8, from a terminal inside the
  desktop) and H3 (the final commit), with the lists in plan section
  12.3.

- **The GUI smoke run** (slice 8). The Tier 2 test `gui_smoke`
  (`crates/cli/tests/tier2_gui.rs`) runs `spherecut --smoke <clip>
  --smoke-seconds 20 --smoke-out <json> --smoke-screenshot <png>`
  (D40) on the noisy 5.7K 8- and 10-bit perf clips, a seam clip
  (synth's seam pattern, 3840×1920 HEVC, 10 s), the sync clip
  (`sync-flash-beep`), each stitched sample (probe's
  `EquirectTagged`) and the largest X5 raw file with two lenses (its
  raw view). A clip that isn't there is listed as not run, with why.
  - **The session** (`tier2::require_graphical_session`):
    `WAYLAND_DISPLAY` when set; otherwise, from a TTY, the single
    `wayland-*` socket the user owns in `XDG_RUNTIME_DIR` (or
    `/run/user/<uid>`, which then goes to the child too), set for the
    child only; otherwise `DISPLAY`; otherwise TIER2-SKIP: "no graphical
    session: run from a terminal inside GNOME or Hyprland, or leave the
    desktop unlocked and on screen". Two sockets aren't guessed between:
    the skip names them.
  - **A run fails** when the process fails or its document isn't `ok`;
    when under 90% of the UI frames expected were presented (the
    monitor's refresh rate, from winit, × the seconds played), which
    means the compositor didn't draw (a locked session, or one on
    another VT: eframe then runs only `logic`, every 250 ms), and then
    nothing else is judged; at over 0.1% of the frames dropped; at UI
    p99 over 1.5× the refresh period; at |A/V| p95 of 20 ms or more on
    the sync clip; or when an HEVC stream decodes in software where
    doctor's check (run in the test) says VA-API decodes its profile.
  - **The report** (`xtask/src/gui_report.rs`): the test leaves each
    run's document and screenshot, and `gui-smoke.json` with the runs
    and their verdicts, in `SPHERECUT_GUI_OUT` (`<target>/hw-gui`,
    cleared before each run). "GUI smoke run" gives a row a run (the
    verdict, presented share, UI p50 and p99, the refresh rate, shown,
    dropped and decode-dropped, the decoder, A/V, audio), then for each
    the adapter, surface format, scale, window and stage times, and its
    screenshot, copied beside the report as `<report>-gui-<n>.png`
    (`hw-reports/<host>-<commit>-gui-<n>.png`).
- **The environment** gains the GUI's display as `gui_smoke` finds it
  (set or discovered, and the variables), the audio server (`pactl
  info`'s server name), GNOME mutter's experimental features
  (`gsettings`), and Hyprland's version, with `pactl info`, `hyprctl
  version` and `hyprctl monitors` in the raw environment.

Why:

- Synthetic grid content decoded at 330 fps in software, so it proved
  nothing about camera footage; the noise and a camera's bitrate make
  the decoder do a camera's work, and the "too easy" flag says when it
  doesn't.
- A per-second minimum catches the VA-API dips (1.17× to 2.41× real
  time on huginn) that a mean hides, and the power settings say whether
  two runs had the machine set the same way.
- FFmpeg 8.1.3 and Mesa 26 behave differently from this box's 6.1.1 and
  25.2.8, so Tier 1 has to run where they are.
- An average bitrate rather than a CRF: at a CRF the noise sets the
  bitrate (about 200 Mbps at CRF 18), and the plan asks for a camera's.
- A stamp of the whole spec, not a version number, so that no change to
  synth's spec can leave an old clip in use.

Tested:

- `xtask/src/machine.rs`: the power settings from a fixture sysfs tree
  of a desktop (an amdgpu card, an Intel iGPU, a connector), and a bare
  one that says "not available" for each; amdgpu's active power profile
  in the old and the new layout; render nodes in order with their
  devices, one that opens and one with no node; lavapipe's ICD where
  the loader looks; rustc's version, and that `MIN_RUST` is the
  workspace's `rust-version`.
- `xtask/src/perf_clips.rs`: two tiny clips are made, then cached; a
  changed spec, or a clip without its stamp, is made again; a clip that
  can't be made says why and leaves nothing behind.
- `hw_test`: Tier 1 from real cargo output, on the scratch workspace
  whose binaries die after their summary or before a result, under
  three sets of user terminal settings; a Tier 1 test that skips has
  failed; a build failure and a cargo failure with no failing binary are
  problems; the perf clips' rows with their bitrate, the "too easy"
  flag and a clip not made; `SPHERECUT_PERF_MEDIA` is set only when
  hw-test makes the clips (not with `--quick`), and its path reaches
  the tests unchanged, as the samples' does.
- synth: an average bitrate is what x264 and x265 make, within 30%, at
  500 and 4000 kbps (`a_noisy_clip_comes_out_at_the_bitrate_asked_for`).
- `cargo xtask hw-test --list` lists the six playback benchmarks among
  31 Tier 2 tests.
- `cargo xtask hw-test --quick --samples …` here, end to end: the
  first run's Tier 1 failed xtask's
  `reads_real_cargo_output_whatever_the_users_terminal_settings`, whose
  control (a user's `[term]` config changes cargo's output when left
  alone) inherited hw-test's `PLAIN_OUTPUT`, which beats the config.
  Its assertion printed the scratch workspace's cargo output, which the
  report then read as 8 binaries of the run's own that "did not
  finish". The control now runs cargo with those variables unset, and
  the test quotes the output it prints;
  `another_cargo_runs_output_in_a_failure_stays_the_failures_when_quoted`
  reads such a failure both ways. The second run passed: Tier 1 589
  passed in 43 binaries, Tier 2 2 passed and 19 skipped, and the
  report has every new section.
- A real perf clip, on this box's 4 CPUs and FFmpeg 6.1 (libx265 3.5):
  hw-test made `hevc8-5k7-30fps-noisy-20s.mp4` in 26 min 52 s, 254 MB
  at 101.6 Mbit/s (HEVC Main, 600 frames, 20.0 s), and software decoded
  it at 27.1 fps (`bench_perf_clips_software_decode`, the median of 3,
  27.06 to 27.19): far from too easy, where the plan's easy grid clip
  decoded at 330 fps and the X5's own 5.7K60 at about 58. The run was stopped there, so the 10-bit and 8K clips were
  not made here; the bench then measured the clip there was and
  recorded the other two as not measured, with the reason.

- Slice 8: the session rule with real sockets in a scratch runtime
  directory (set first, empty as unset, a lock file and a plain file
  aren't sockets, another user's isn't ours, two are named, not
  guessed); each gate on documents made to fail it
  (`the_gates_fail_what_they_say`), the hidden session saying only that;
  the report's section from a fixture run, with the screenshot copied
  and named, and without a summary. Here, with no desktop,
  `SPHERECUT_TIER2=1 cargo test -p spherecut-cli --test tier2_gui --
  --ignored` skips with the reason, and `hw-test --list` lists
  `tier2_gui::gui_smoke` among 39 Tier 2 tests.

Revisit if: H1's perf clips decode in software above 150 fps on either
machine (more noise, or a higher bitrate); x265 can't hold 100 Mbps on
a build Pat uses; the Tier 1 run on a hardware machine takes so long
that `--no-tier1` becomes the habit; or H2's `gui_smoke` fails a
session that was on screen (winit reporting the wrong refresh rate, or
a compositor that paces a visible window below it).

## D38. Vulkan Video is tested on VA-API's GPU, and skips in FFmpeg's words

*2026-10-24, M1. Pat approved Vulkan Video HEVC in odin's hardware run
(the stack review's third change; PLAN M1's hardware criterion).
Extends D14. D27 to D37 are kept for the M1 plan's decisions
(`docs/design/m1-plan.md`, section 0), so this one is D38.*

How FFmpeg picks the device: `av_hwdevice_ctx_create` for Vulkan with
no device string takes physical device 0 of its own Vulkan instance
(`find_device` in `libavutil/hwcontext_vulkan.c`, the same in 6.1.1 and
8.1.3). A number picks that index, other text the first device whose
name contains it. Derived from a VA-API device it matches the PCI device
ID (8.1 with libva 1.15 and up) or only AMD's vendor ID (6.1); derived
from a DRM device it compares the `st_dev` of the node's `fstat`, which
is the device of `/dev` itself, not the node's `st_rdev`. The order of
the list comes from the Vulkan loader and its layers. Current Linux
loaders list discrete GPUs, then integrated, virtual, other and CPU
devices, then by PCI address (`loader_linux.c`). Mesa's device-select
layer (in `mesa-vulkan-drivers`) then moves its default to the front:
one named by `MESA_VK_DEVICE_SELECT` or `DRI_PRIME`, else the GPU the
Wayland compositor or X server uses, else the boot VGA device, else the
first that isn't a CPU. So FFmpeg's default is a GPU whenever one
works, but with two GPUs it is the one that runs the desktop (over SSH,
the boot VGA device), which need not be the one behind VA-API's
`renderD128`. odin has an RX 7600 and may have an integrated GPU too.
llvmpipe comes first only when no GPU driver works, or with a loader too
old to sort and no device-select layer.

So `decode::open_vulkan_device(render_node)` lists FFmpeg's devices,
with their DRM nodes from `VK_EXT_physical_device_drm`, and opens the
one behind the render node, else the first that isn't a CPU, else
FFmpeg's default. It opens that one by index and checks that FFmpeg then
runs on it (an error if not, never a silent other device), and says
which device it is and why (`VulkanChoice`, whose `Display` names the
device, its type and driver, and FFmpeg's default when that was another
one). The Tier 2 Vulkan tests and benchmarks use it with
`tier2::render_node()` (`SPHERECUT_RENDER_NODE`, default
`/dev/dri/renderD128`, when present) and skip when the device runs on
the CPU, naming it. They no longer need the render node itself.

When the driver can't decode a format (`HwUnsupported`), a test skips
with the device and the lines FFmpeg logged while it tried, such as
huginn's "Device does not support decoding hevc!". They are caught with
`capture_hw_decode_log`, which also keeps the few lines FFmpeg's Vulkan
decoder logs at "verbose" about a stream it turns down: "Unable to
initialize video session: hevc profile "Main 10" not supported!" for a
profile or pixel format the driver lacks, and, since a stream too big or
of too high a level is refused without a word, the limits it checked
("Maximum level: 153 (stream 186)", "Width: from 64 to 4096"). The skip
also gives the stream's size and level. Any other error fails it,
including FFmpeg failing to open the GPU chosen. The samples test
groups refused streams by format and reason, fails on any mismatch or
other error, and otherwise skips if any stream was refused. A
benchmark's Vulkan Video row is measured only when the file's first
frames decode on the device, through `bench::try_measure`, which records
nothing when a run fails. Otherwise `bench::not_measured` leaves a note
with the reason, which the hw-test report lists under its benchmark
table, so a missing Vulkan path never fails a VA-API benchmark. Another
note names the device the Vulkan rows ran on.

Doctor still checks FFmpeg's default Vulkan device and names it (its
JSON marks it `used_by_ffmpeg`), so on a machine with two GPUs its
Vulkan lines can be about another GPU than the tests'.

Fedora's Mesa: its Vulkan drivers are built without H.264 and HEVC,
like its VA-API driver. RADV then lacks `VK_KHR_video_decode_h264` and
`_h265`, which is what "Device does not support decoding hevc!" means
(`vulkan_decode.c`), while AV1 decodes, so video decode itself is on and
`RADV_PERFTEST=video_decode` changes nothing. RPM Fusion's
`mesa-vulkan-drivers-freeworld` (built with `video-codecs=h264dec,
h265dec,...`, and conflicting with `mesa-vulkan-drivers`) has them. So
on Fedora's stock Mesa, odin's Vulkan H.264 and HEVC checks skip as
huginn's did. Doctor's fix for a Mesa GPU on Fedora that can't decode
H.264 or HEVC with Vulkan Video is now that package, as for VA-API;
whether odin's run swaps it in first is Pat's call.

Why: odin's discrete RDNA3 is the first chance to compare Vulkan Video
HEVC with VA-API (with the freeworld driver; huginn's stock Vulkan
driver has no HEVC). On FFmpeg's default device, odin's run could
have measured the integrated GPU, or skipped as if Vulkan Video were
missing when only the device was wrong; and the old skip said only that
the driver doesn't support the format, not FFmpeg's reason.

Revisit if: doctor or the app should decode on the same device (the app
will, once it decodes on Vulkan), or FFmpeg changes how it picks one.

## D39. `frame --decoder auto` tries VA-API on the rendering GPU, then software

*2026-10-24, M1 slice 4. The order the M1 plan's section 5 gives (step
6), with the node from wgpu's adapter (section 3.5). D27 to D37 are
kept for the plan's decisions and D38 is taken, so this one is D39.
Updates D30.*

What `--decoder` does in `spherecut frame`:

- **The render node** is `--render-node` when given, else the node of
  the GPU that renders: `media::hw::render_node` with the adapter's
  PCI bus id, vendor and device from wgpu's `AdapterInfo` (D30's
  lookup: bus id, then the two ids, then `/dev/dri/renderD128`). A CPU
  adapter (lavapipe) and `--reference` give no ids, so the default node.
- **`auto`**, the default, decodes on VA-API on that node with
  `Fallback::Software` (D30): a device that can't be opened, turns the
  stream down or fails on a frame hands over to software, and the
  result says so in `decode.fallback` (`from`, `code`: `hw_unavailable`,
  `hw_unsupported` or `hw_failed`, and `reason`). When the node doesn't
  exist, `auto` doesn't ask FFmpeg at all: it decodes in software with
  the fallback `hw_unavailable`, "no VA-API device: no render node at
  <path>", so a machine without a GPU never shows an FFmpeg error for
  it. `auto` never picks Vulkan Video.
- **`vaapi` and `vulkan`** decode on that device with `Fallback::Fail`,
  and its error is the run's, exit 3 (D27). `vulkan` opens the Vulkan
  device behind the node through `open_vulkan_device` (D38), and refuses
  one that runs on the CPU, as lavapipe does, with `hw_unavailable`.
- **`software`** decodes in software, with no fallback to report.

The fallback is not one of the document's `warnings`, though D30 had
"frame's stderr note and JSON warning": it is part of the result, where
an agent reads which path a frame took (amendment 2 asks for that), and
the frame is the same either way, since software and VA-API decode
H.264 and HEVC bit for bit alike (D30's Tier 2 checks). On a machine
without VA-API every run would otherwise carry the same warning. A
device that was tried and failed is also on stderr, as the backend's
`tracing::warn!` ("decoding in software instead of with VA-API:
<reason>"), and the text line names the fallback: "decoded in software
(VA-API: no VA-API device: no render node at /dev/dri/renderD128)".

Why this order: VA-API is the hardware path Spherecut relies on (D3,
D30), and decodes H.264 and HEVC on both of Pat's machines bit for bit
like software; Vulkan Video is optional (D38), and huginn's stock RADV
can't decode either. One frame costs one download, so the copy that
makes VA-API slower than Vulkan Video on odin's discrete GPU (amendment
9) doesn't matter here. The node comes from the GPU that renders, so
that on a machine with two GPUs the frame is decoded and rendered on
the same one. The Tier 2 `vaapi_render_matches_software`
(`crates/cli/tests/tier2_frame.rs`) runs `auto` with no `--render-node`
(unless `SPHERECUT_RENDER_NODE` names one) and checks that it decoded
on VA-API with no fallback, on a node frame's info log says it found
from the adapter's PCI bus id or ids, not the default. That shows the
lookup ran on the rendering adapter's ids; the result doesn't name the
node, so no test checks that it is the same GPU beyond `media::hw`'s
own tests of the lookup on fake sysfs trees.

Tested: `crates/cli/tests/frame.rs` (`--decoder vaapi --render-node
/nonexistent` and `--decoder vulkan` on lavapipe, the Vulkan loader
shown lavapipe's ICD alone, are `hw_unavailable`, exit 3, with nothing
written) and `crates/cli/tests/contract.rs` (the
same for exit 3's document); every other run of those tests is `auto`
with the no-node fallback, and the envelope test checks the
`decode` it reports.
`vaapi_render_matches_software` asserts the hardware path with no
fallback, for `vaapi` and for `auto`, and `vulkan_render_matches_software`
skips, with FFmpeg's lines, where the driver turns a clip down (D38).
`auto`'s fallback after a device that turns the stream down
(`hw_unsupported`) or fails on a frame (`hw_failed`) has no test of
frame's in any tier yet: Tier 1 has no device, and the Tier 2 tests
expect the device to decode. The backend's fallback itself is tested
with injected faults (`crates/synth/tests/fallback.rs`, D30).

Revisit if: slice 5 picks the hardware kind from doctor's checks for
playback (amendment 9); `auto` should then use the same choice, so that
it takes Vulkan Video where doctor shows that it works and VA-API
doesn't.

*Slice 5 does (D34): the player's `Auto` picks from doctor's checks.
When both kinds decode the format it takes Vulkan Video on a discrete
GPU and VA-API on any other, and when only one does, that one. `frame
--decoder auto` keeps VA-API, then software. Its choice from doctor's checks would mean running doctor
(every device and decode check, and the encode checks) in each `frame`
run, which costs more than the one frame it decodes. Where only
Vulkan Video decodes, `frame` still gives the frame, decoded in
software. Revisit when the checks are kept where `frame` can read them
(the GUI's Settings), or when a frame on such a machine is too slow.*

## D40. The overlay, the settings, the raw view and the smoke run

*2026-10-30, M1 slice 8. The M1 plan's sections 7.1, 7.8, 8.2, 10 and
12 (item 5), as built. The plan gives them no entry of their own; this
one records the choices it left open. Updates D34, D35 and D36.*

What:

- **One source of numbers** (D34). The overlay, the smoke run and the
  benchmarks read the same `PerfStats`. Slice 8 adds the counters
  `projected` and `encoded_only` (the viewer's draws), the projection's
  CPU and GPU time (`render_cpu`, `render_gpu`), and `MemoryStats`,
  which the upload thread reads once a second: `VmRSS` from
  `/proc/self/status`, minor faults from `/proc/self/stat` (and a rate
  from the reading before), and wgpu's allocator report.
- **The overlay** (F3, plan section 10) is monospace labels in an
  `egui::Area` over the viewer, so a screen reader would read them
  (D36's accesskit tree) and kittest queries them, with sparklines of
  the last samples. Rates (frames decoded, the draws that only encoded)
  are over the last second of UI frames, from the counters each frame
  read; the A/V offset's median and p95 of its size are over the last
  second's samples. The projection's GPU time is `GpuTimers`', read a
  frame or two late and never waited for; on lavapipe it says "(CPU
  timing)", and without `TIMESTAMP_QUERY_INSIDE_ENCODERS` "n/a".
- **Settings** (plan sections 7.1 and 8.2) are a `Settings` in eframe's
  storage beside the tiles, serde with `#[serde(default)]` so a copy
  from an older version loads, and `sanitized()` brings each value into
  the window's range (cache 256 MiB to 64 GiB or automatic, slots 2 to
  6, A/V offset ±500 ms). A change goes to the player at once
  (`set_config`): the cache's budget and the slot count as the upload
  thread reaches them, the A/V offset from the next frame, the decoder
  for the files opened after. `--cache-mb` and
  `SPHERECUT_FRAME_CACHE_MIB` win over the setting (`CacheBudget::choose`
  says which), and the window then greys the field out with "set by …".
- **The raw view** (plan section 7.8). A source that isn't equirect, or
  a camera's raw file whose lenses are separate video streams, is shown
  flat, a column a stream, by render's flat pass (`fs_flat`: the
  picture's own texels with the clamp sampler, as `render_still`'s
  `StillOptions.flat` renders them, checked against
  `reference::render_flat`). Each column has its own `LinearTarget`,
  sized by `viewer::raw_place`, painted under a scissor of its own,
  since the encode pass blacks out the rest of its viewport. The look
  drag is off. `Player::open_streams` needs the streams' indices up
  front, which only a probe gives, so the file opens on its best
  stream, and when the Info pane's probe finds two or more video
  streams of one size (cover art aside) the app opens it again with all
  of them and raises a `Lenses` notice.
- **The smoke run** (`spherecut --smoke`, plan section 12 item 5,
  `docs/cli-json.md`). `smoke::SmokeDriver` rides the app's passes: it
  counts every pass in `logic` and the UI's in `ui`, starts playing at
  the first frame shown, and at the end builds a `spherecut.smoke/1`
  document in the CLI's envelope.
  - The stages' percentiles are over the whole run, not the rings' last
    256 samples: each pass takes the samples recorded since the one
    before (`PerfStats::samples_after`), and counts as `missed` those a
    ring's worth too late to read.
  - The UI frames expected are the monitor's refresh rate (winit's
    `refresh_rate_millihertz`, read each pass) times the seconds played,
    or the passes when winit doesn't know the rate (kittest); a hidden
    window runs passes without the UI, so its `presented` share falls.
  - The screenshot is `ctx.request_screenshot`'s, kept to 960 pixels
    wide with a triangle filter. It and the document are written on a
    worker through `TempFile` (whole, or the file as it was), so the UI
    thread never waits on the disk.
  - The run uses the default settings with the overlay on, and neither
    reads nor writes the stored settings, the layout or the window's
    place (`persist_window` off). Its failures exit by their codes: the
    player's (`not_found`, ...), `timed_out` (a new code: no frame within
    60 s), `cancelled` (the window closed before the end), `io_error`
    and `internal`.
  - *(Slice 9.)* A watchdog thread (`SmokeDriver::watch`, run.rs starts
    it) ends a run whose window has had no UI pass for 15 s
    (`smoke::STALL`): it writes a `timed_out` document saying how long
    after the window opened, in which phase, and whether eframe's logic
    still ran, then calls `std::process::exit(1)`. The driver beats it
    from `ui` and `logic`; once the document is being written, or the
    window is closing, it writes nothing and exits 1 only if the process
    is still there 15 s later. hw-test's `gui_smoke` runs nothing while
    logind's `LockedHint` says the session is locked
    (`tier2::session_locked`), fails at once when it is at the start,
    and says on a failed run whether the screen had locked by its end.

Why: hw-test's report, the overlay and `docs/perf.md` have to agree,
which one set of counters gives. The watchdog came from H2 on odin
(2026-10-30): the screen had locked after 2.5 hours idle, every smoke
run's window got no UI pass, so neither the 60 s first-frame limit nor
the run's end, both checked in UI passes, ever came, and `gui_smoke`
killed each run after 120 s with no document. The UI thread may be
stuck in presenting, where no timer of its own can fire, so the
watchdog is a thread of its own and exits the process rather than ask
the window to close; `std::process::exit` over `libc::_exit` keeps the
app free of unsafe code, at the risk of an exit handler that waits on
the stuck thread, which `gui_smoke`'s 120 s kill still covers, the
document already written. A ring's percentiles would describe the
last four seconds of a 20 s run, which is how a hitch at the start goes
unseen. A refresh rate from the compositor is the only way to tell a
hidden window from a slow one: a window the compositor doesn't draw
gets no frame callbacks, and eframe then runs only `logic`, every
250 ms. Writing on a worker keeps the smoke run under the GUI's own
never-block rule. Reopening with the lenses costs one open, where
waiting for the probe before the first open would delay every file's
first frame by the probe.

Tested (kittest on lavapipe, `crates/app/tests`):

- `overlay.rs`: after 2 s of playing, every line has its number
  (decode, upload and render on the CPU and through lavapipe's
  timestamps, UI frames, frames shown, draws, the viewer's size and
  scale, the cache, the slots, RSS and faults), and F3 takes it away.
- `settings.rs`: each setting reaches the player's config at once, past
  the range lands on the range's end, the budget in force is shown,
  Restore defaults keeps the folder; a cache the flag set stays its own,
  greyed out. Unit tests: an old stored copy and a wild one load.
- `raw.rs`: a two-lens Matroska file (`media::encode::mux_video_streams`)
  opens on one stream, then both; each column is pixel for pixel the
  flat still of its frame at `raw_place`'s place, black around them
  (a paint one pixel off fails it, checked by mutation); a drag leaves
  the view; the grid equirect after it is projected again. render's
  `the_flat_pass_matches_the_reference`: 52.76, 50.73 and 52.38 dB
  against floors of 49, 47 and 49, with a control at 10 dB.
- `smoke.rs`: the run plays, measures, shoots and writes, every field
  checked (the screenshot 960 × 600 from a 1600 × 1000 window, no stage
  sample missed, presented 1.0 with no refresh rate); a clip that can't
  open ends the run with `not_found`. The watchdog (slice 9): with no
  pass at all it writes `timed_out` saying the UI thread is stuck, and
  exits 1; with logic alone it says that logic ran; a finished run's
  document is left alone, and only the exit comes after the stall.
  testkit's `the_lock_is_read_from_the_users_graphical_session` checks
  the `loginctl` rule, and xtask's report test the locked summary. `contract.rs`: the hidden flags'
  usage errors, `no_display` before any window, and not in `--help`.

Revisit if: H2's screenshots show the overlay unreadable over bright
footage; a camera writes its lenses in streams of different sizes (the
raw view's reopen then doesn't fire); M3's stitcher replaces the raw
view; or winit stops reporting the refresh rate on a compositor Pat
uses.

## Open

- **telemetry-parser** (Gyroflow's camera metadata parser). DJI Osmo 360
  support is only on its git master, and `deny.toml` rejects git sources.
  Options for M4: vendor the parts we need at a pinned commit with their
  license, allow that one git revision in `deny.toml`, or write our own
  parsers from `docs/formats/`. To decide at M4, after checking its license
  and what we actually need from it.
- **MCP layer, tool schemas, openat2** for agent integration: rmcp or our
  own layer, schemars or hand-written, rustix or `libc`. To decide at A1
  (D23).
- **Attach socket** (A3): Pat to confirm that a local socket the GUI
  opens only when he turns attach on (mode 0600, his user only) is IPC,
  not the network access his rule forbids. Before A3 (D23).
