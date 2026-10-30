# Spherecut

A native Linux editor for 360° video, written in Rust, in early
development. It is being built to open raw files from Insta360, GoPro and
DJI 360 cameras as well as stitched equirectangular video, to reframe them
into flat video or edit them as 360, and to export with the metadata
YouTube and headsets need. It will render with wgpu on Vulkan. It decodes
with FFmpeg, in software or on the GPU through VA-API.

It runs offline: no accounts, no telemetry, no network access.

**Status: early.** Milestone M0 (foundations) is done; M1, the viewer, is
in progress. What works today:

- `spherecut [file]` opens the viewer: a stitched equirect played on the
  GPU with its audio, looked around by dragging (the point you grab stays
  under the pointer), zoomed about the pointer, shown rectilinear,
  fisheye or as a tiny planet, stepped frame by frame and scrubbed. It
  checks your codecs and GPU at startup and shows the fixes it finds.
  F3 shows the performance overlay, File ▸ Settings keeps the cache,
  decoder, filter, slots and A/V offset, and a camera's raw file with a
  stream a lens shows its lenses flat, side by side.

- `spherecut probe <file>...` shows what an MP4/MOV or Matroska/WebM file
  is: container, streams, codecs, colour, exact frame rates and
  timebases, spherical video and ambisonic metadata, and the camera
  vendor and layout it most likely came from. It also checks MP4/MOV
  files for the kinds of damage listed under Build and run. `--json`
  gives the same as one JSON document, for scripts and AI agents
  ([docs/cli-json.md](docs/cli-json.md)).
- `spherecut doctor` checks what your FFmpeg, drivers and GPU can actually
  do. It decodes one small frame each of H.264 High, HEVC Main, HEVC Main 10
  and AV1 in software, on VA-API and on FFmpeg's Vulkan device, encodes one
  frame with each VA-API encoder, and lists the Vulkan devices. For each
  problem it prints a fix: the exact `dnf` or `pacman` command where a
  package fixes it.
- `spherecut frame <file> -o out.png` renders one frame of a stitched
  equirect through a virtual camera (yaw, pitch, roll, field of view,
  rectilinear, fisheye or stereographic, and a morph between two) into a
  PNG, headless, on the GPU's Vulkan driver or Mesa's lavapipe. The frame
  is exactly the one asked for, by time or by number.

See `PLAN.md` for the milestones and `PROGRESS.md` for where things stand.
The plan also has agent integration: an MCP server and JSON output so that
AI agents such as Claude Code, Codex or Gemini CLI can use Spherecut.
Spherecut itself still makes no network requests, but what it returns to
an agent, frames included, goes to that agent's model provider.

## Requirements

- Linux with an AMD or Intel GPU (Mesa RADV or ANV). NVIDIA isn't supported.
- FFmpeg 6.1 or newer, with its development files, H.264/HEVC decoders,
  an AV1 software decoder (libdav1d), and libx264/libx265, built with
  Vulkan support (the Tier 1 doctor and hardware-refusal tests need
  FFmpeg's Vulkan device).
- Rust 1.95 or newer, with rustfmt and clippy for development.
- clang, for generating the FFmpeg bindings at build time.
- The ALSA library's development files (libasound) and pkg-config, which
  the audio output links. Sound plays through PulseAudio's protocol,
  which PipeWire serves on Fedora and Arch (`pipewire-pulse`), or else
  ALSA; with no sound device, playback is silent.
- For the viewer, a Wayland or X11 desktop session. It loads
  libwayland-client and libxkbcommon (Wayland), or libX11, libXcursor,
  libXrandr, libXi and libxkbcommon-x11 (X11), when its window opens, as
  it loads the Vulkan loader; a desktop has them. With no display,
  `spherecut` exits 3 and says so, and the other commands work as
  before.
- For the Tier 1 tests: Mesa's lavapipe Vulkan driver and strace (see
  Development).

### Fedora (43 or newer)

Fedora's own FFmpeg (`ffmpeg-free`) and Mesa VA-API driver leave out H.264
and HEVC, which nearly every 360 camera records. Use RPM Fusion's builds
(enable the RPM Fusion free repository first; see rpmfusion.org):

```sh
sudo dnf swap ffmpeg-free ffmpeg --allowerasing  # or, without ffmpeg-free: sudo dnf install ffmpeg --allowerasing
sudo dnf install ffmpeg-devel clang-devel pkgconf-pkg-config alsa-lib-devel libva-utils vulkan-tools mesa-vulkan-drivers
sudo dnf install strace                          # for the Tier 1 tests
# AMD GPUs:
sudo dnf install mesa-va-drivers-freeworld       # or: dnf swap mesa-va-drivers mesa-va-drivers-freeworld
# Intel GPUs (RPM Fusion nonfree):
sudo dnf install intel-media-driver
# Rust with rustfmt and clippy:
sudo dnf install rustup && rustup-init -y
```

### Arch

```sh
sudo pacman -S --needed rust ffmpeg clang pkgconf alsa-lib libva-utils vulkan-tools
sudo pacman -S --needed vulkan-radeon                       # AMD
sudo pacman -S --needed vulkan-intel intel-media-driver     # Intel
sudo pacman -S --needed vulkan-swrast strace                # lavapipe and strace, for the Tier 1 tests
```

### Ubuntu 24.04 (development only)

```sh
sudo apt install pkg-config clang ffmpeg libavcodec-dev libavformat-dev \
  libavutil-dev libswscale-dev libswresample-dev libasound2-dev mesa-vulkan-drivers \
  vulkan-tools vainfo strace
```

Your user needs to be in the `render` group for GPU video
(`sudo usermod -aG render "$USER"`, then log in again). `spherecut doctor`
checks the FFmpeg codecs, the GPU drivers and access to the render node.

## Build and run

```sh
cargo build --release
./target/release/spherecut clip.mp4
./target/release/spherecut doctor
./target/release/spherecut probe VID_20261022_165451_00_004.insv
./target/release/spherecut probe --json clip.mp4
./target/release/spherecut frame clip.mp4 -o out.png --time 12.5 --yaw 30 --fov 90
```

`spherecut probe` exits 0 when every file is healthy, 1 when any file
can't be opened, isn't MP4/MOV or Matroska/WebM, is truncated or corrupt,
or has no video or audio stream, and 2 when the command line doesn't
parse, such as `spherecut probe` with no file.
A damaged file that FFmpeg can open still gets its report, with a
`DAMAGE` line per problem, and stderr gets one line per bad file, such as
`error: clip.mp4 is damaged: <first reason> (and 2 more problems)`. In
MP4/MOV files, probe counts these as damage:

- a top-level box cut off by the end of the file, which the `mp4` line
  names: ``truncated `moov` (4847 of 4947 bytes)``;
- anything the box reader warns about, such as a malformed box, a box
  below the top level whose size is 0 (only the last top-level box may
  have one; anything after it in its parent is read as part of it), and a
  missing `moov` box. The reader looks inside `moov` and small top-level
  containers such as `udta`, not inside fragments (`moof`);
- sample tables that disagree with each other, or chunks outside the
  `mdat` data;
- a box type that isn't printable text inside `moov`, `trak`, `mdia`,
  `minf`, `stbl`, `stsd`, `edts`, `dinf`, `mvex`, `tref`, the sample
  entries probe knows, `sv3d` or `proj`, including four zero bytes
  anywhere but in QuickTime's
  terminator, the last box of a sample entry, which holds only zero bytes;
- a box in `moov` that starts with a track header (`tkhd`) but whose type
  is another printable one, such as `Trak` or `free`: FFmpeg skips it, and
  its stream with it;
- an `sv3d` without its `svhd`, its `proj` or the `prhd` in `proj`, which
  makes FFmpeg ignore the projection;
- a box in a sample entry that starts with a spherical video header
  (`svhd`) but whose type is another printable one, such as `rv3d`: FFmpeg
  skips it, and the projection with it;
- a `mvhd` or `mdhd` timescale of 0, or one FFmpeg reads as negative;
- a video sample entry under an audio handler, or the other way round;
- a video or audio track that FFmpeg reads as another kind of stream;
- a video or audio stream FFmpeg could not set up although its track has
  samples and its sample entry is a codec FFmpeg reads, which usually
  means a damaged `avcC` or `hvcC`;
- a video or audio stream whose decoder refuses to open with its codec
  setup, such as an `esds` that gives a reserved sample rate, although
  FFmpeg filled in the stream's fields from the sample entry.

Damage that keeps the structure consistent, such as changed sample sizes
or times, and damage inside `mdat` are not found without decoding. A
stream whose codec FFmpeg doesn't know, such as a `zzzz` sample entry, is
a `warning` line and doesn't fail probe: it may be a real codec that this
FFmpeg lacks. So is a stream this FFmpeg has no decoder for, a stream
FFmpeg could not set up from an empty track, a stream whose decoder
says it lacks a feature the stream uses (`Function not implemented`, as
for AAC object type 0), and a stream of pictures larger than the 2^27
pixels (16384x8192) Spherecut's decoders make (DECISIONS D24): the
warning gives its size. The `ffmpeg` lines hold the warnings and errors
FFmpeg logged while reading the file, one whole line each, starting with
the context that logged it, such as `[aac]`. A repeated line is listed
once with a count, and at most 32 are listed. They often say what FFmpeg
could not read, but they don't fail probe either, because healthy files
can have some: cover art gives a `timescale not set` line. Only MP4/MOV
structure is checked in full; Matroska files are checked against their
Segment length only, which a `structure` line says.

Probe opens nothing but MP4/MOV and Matroska/WebM files, and FFmpeg reads
only the file it was given. A file FFmpeg would read as anything else,
such as an HLS playlist (`.m3u8`), an ffconcat script or a WAV file, is
refused, with the container's name, before FFmpeg's demuxer for it
runs: ``error: FFmpeg reads list.m3u8 as Apple HTTP Live Streaming
(`hls`), and Spherecut opens only MP4/MOV and Matroska/WebM files``. A
file name is always a file name, even one like `concat:a.mp4` or
`http://host/x.mp4`, and FFmpeg gets no network protocols (DECISIONS
D24).

When a file can't be opened, its `ffmpeg` lines follow the error on
stderr (with `--json`, they are in the file's `error.ffmpeg_log`), since
FFmpeg's log often says why it could not open the file (`moov atom not
found`). File names don't have to be UTF-8; in the text
report and in the JSON, bytes that aren't UTF-8 show as U+FFFD. When the
reader of probe's output goes away, as in `spherecut probe *.mp4 |
head`, the output stops quietly, and probe still checks the remaining
files, so the exit code and the errors on stderr cover every file.

`spherecut doctor [--render-node <PATH>] [--json]` checks the GPU behind
the render node hardware decoding uses, which is `/dev/dri/renderD128`
until the GUI knows the GPU it renders on (D30), or the one
`--render-node` names. (`spherecut frame` decodes on the node of the GPU
it renders on, D39.) It exits 1 only when a problem is an error: FFmpeg has no H.264, HEVC, ProRes or AAC
decoder, or nothing decodes AV1. Every decoder and encoder in doctor's
tables that FFmpeg lacks gets a problem that names it, such as `FFmpeg has
no software AV1 encoder (libsvtav1)`, with the fix for FFmpeg; an FFmpeg
built without VA-API gets one problem saying so instead of one for each
VA-API encoder. A missing Opus or PCM decoder, or a missing AAC, libx264,
libx265, libsvtav1, VA-API H.264 or VA-API HEVC encoder, is a warning. A
missing `av1` decoder (FFmpeg's own, which VA-API and Vulkan Video use),
VA-API AV1 encoder or ProRes encoder is a note. Warnings, such as no GPU
or a Vulkan device that runs on the CPU, exit 0. Doctor tells a driver
that refuses a format (`cannot decode HEVC Main 10`) from one that decodes
it wrongly (`decodes HEVC Main wrongly (N of M samples differ from
FFmpeg's software decoder)`); for the second, the fix is a newer driver.
In `--json`, a failed check's `fault` is `ffmpeg`, `driver` or
`wrong_output`. Doctor never installs or changes anything; on Fedora it
makes one read-only `rpm -q` query to word its fixes.

### The viewer

```sh
spherecut                                  # the viewer, with its Files pane to open one
spherecut clip.mp4                         # clip.mp4 shown
spherecut clip.mp4 --cache-mb 4096         # a 4 GiB frame cache (default: a quarter of free memory, 512 MiB to 8 GiB)
spherecut clip.mp4 --adapter cpu           # render on lavapipe (or part of a GPU's name, or its PCI bus id)
```

Drag the picture to look around; with Ctrl held the drag is free to
roll. Scroll (or Ctrl+scroll) to zoom about the pointer, or + and − to
zoom about the centre.
Touchpad pinch doesn't reach applications under winit on Linux yet, so
two-finger scrolling zooms. 1, 2 and 3 switch to rectilinear, fisheye
and tiny planet, and R looks forward and level again. Space or K plays
and pauses, Right and Left (or . and ,) step a frame, Shift with them
jumps a second, Home and End go to the first and last frames, and
dragging the bar under the picture scrubs. F3 shows the overlay: the
decode path, each stage's time on the CPU and the GPU, frames shown
and dropped, the A/V offset, the cache, the slots, memory and the
audio output. File ▸ Settings changes the cache's budget, the decoder,
the filter, the GPU slots and the A/V offset at once, and keeps them;
`--cache-mb` and `SPHERECUT_FRAME_CACHE_MIB` win over the setting. The
Files pane opens videos (there is no file dialog, and files can't be
dropped on the window under Wayland). The Info pane shows the file's
probe report, and the Doctor pane the startup check's whole report; its
errors and warnings also show as banners above the viewer, each with its
fix and a Copy button. `SPHERECUT_FRAME_CACHE_MIB` and
`SPHERECUT_ADAPTER` set the cache and the GPU as the options do.

### Rendering a frame

```sh
spherecut frame clip.mp4 -o out.png                       # frame 0, 1920x1080, rectilinear 100°
spherecut frame clip.mp4 -o out.png --time 1:02.5 --yaw 90 --pitch -20 --roll 5
spherecut frame clip.mp4 -o out.png --frame 370 --projection fisheye --fov 360 --size 1024x1024
spherecut frame clip.mp4 -o out.png --time 30000/1001 --projection stereographic --fov 300 --pitch -90   # a tiny planet
spherecut frame clip.mp4 -o out.png --morph-to stereographic --morph 0.5 --fov 150
spherecut frame clip.mp4 -o out.png --overwrite --depth 16 --json
```

- **When**: `--time` takes seconds (`12`, `12.5`), `m:s` or `h:m:s`
  (`1:02.5`, `01:02:03.250`), a rational (`30000/1001`) or a frame
  number (`370f`), all read exactly, in clip time: 0 is the first frame
  shown, whatever the container's start time or edit list. The frame
  shown is the one on screen at that time. `--frame N` is `--time Nf`,
  counting from 0; on a variable-frame-rate file it counts frames from
  the start. A time past the last frame fails, naming the clip's range,
  and so does a frame number past it. A frame the file has lost, cut
  short or with frames missing, fails as damage rather than showing
  another frame in its place.
- **The camera**: `--yaw` (degrees right), `--pitch` (up) and `--roll`
  (clockwise, seen from behind the camera), all 0 by default;
  `--fov`, horizontal, at most 170 rectilinear, 360 fisheye and 340
  stereographic; `--size WxH`, 16 to 16384 a side and within the GPU's
  limit; `--depth 8|16` for the PNG.
- **The file** must be tagged as an equirect (Spherical Video V1 or V2
  metadata in an MP4 or MOV file); `--equirect` renders an untagged one
  as an equirect anyway. Matroska/WebM files need `--equirect` for now:
  Spherecut doesn't read their spherical metadata yet.
  Dual fisheye files from the cameras wait for stitching in M3. Stereo
  files render the left eye, a cropped picture shows black where it
  doesn't cover the sphere, and a pose in the metadata isn't applied
  yet; each says so in a warning.
- **The GPU**: `--adapter auto` (the default, or `$SPHERECUT_ADAPTER`)
  takes a GPU and falls back to lavapipe; `cpu` is lavapipe, and other
  text picks an adapter by part of its name or its PCI bus id.
  `--reference` renders with the slow f64 CPU reference instead.
  `--filter aniso` is sharper where the view squeezes the source, and
  the driver's own; `iso`, the default, is the same on every driver.
- **Decoding**: `--decoder auto` (the default) decodes with VA-API on the
  render node of the GPU that renders, and in software when that can't
  (no render node, a stream the GPU turns down, a failure), and says
  which; `software`, `vaapi` and `vulkan` force one, and the hardware
  ones fail instead of falling back (DECISIONS D39). `--render-node`
  names the node. `--zero-copy` keeps the frame on the device: VA-API
  exports it as DMA-BUFs and the GPU imports them, with no copy through
  the CPU; it fails (exit 3, `import_failed`) when the GPU, its driver
  or the decoder can't, and the JSON's `decode.transfer` says which way
  a frame came (slice 5b, DECISIONS D25). The viewer does the same with
  `SPHERECUT_ZERO_COPY=1`, and copies whatever it can't import.
- **The output**: something already at `-o` is refused unless
  `--overwrite` is given, and so is a link there; with `--overwrite` a
  regular file is replaced, through a link too. The PNG is written next
  to the output and linked into place whole (with `--overwrite`, renamed
  into place), so a failed or interrupted run leaves nothing behind, and
  a file that appears at the output while the frame renders is kept
  (DECISIONS D16).
- **Ctrl+C** ends the run with exit 4 and no output. It stops a seek or
  a read at once; during probe, the GPU's set-up, the render or the
  PNG's encode it takes effect when that step ends, which at a large
  `--size` can take seconds. A second Ctrl+C kills it at once. Started
  with SIGINT ignored, as a script's background job (`cmd &`) is, frame
  leaves it ignored.

On success it prints one line: `wrote out.png: 1920×1080 rectilinear
100° at 12.5 s (frame 375, pts 375000), decoded on VA-API, rendered on AMD
Radeon 8060S (RADV)`. It exits 0 when the PNG is written; 1 when the
file, the frame or the output is the problem (missing, damaged before
the frame, not an equirect, past the end, the output in the way); 2 for
a command line that doesn't parse or a value out of range; 3 when a
device or codec it needs is missing (no Vulkan driver, no adapter by
that name, no decoder for the codec, or the hardware `--decoder` named
not working, which `spherecut doctor` explains); and 4 when interrupted.

### JSON output

`spherecut probe --json`, `spherecut doctor --json` and `spherecut frame
--json` print exactly one line of JSON on stdout per run, on success and on failure, and keep
stderr for the log. [docs/cli-json.md](docs/cli-json.md) documents it
field by field, with the codes, the exit statuses and an example per
command; DECISIONS D27 says why it is this way. In short:

```json
{"schema":"spherecut.probe/1","ok":false,"result":{"files":[...]},"error":{"code":"files_failed","message":"..."},"warnings":[]}
```

- `schema` names the document and its version. Within a version, fields
  are only ever added.
- probe's `result` is `{"files": [...]}`, for one file as for many: each
  has its `path`, `ok`, its `report` whenever FFmpeg could open it, and
  its `error` when it fails. A damaged file has both.
- An error is `{"code", "message", ...}`; the code (`truncated`,
  `not_found`, `not_media`, `usage`, ...) is stable, the message is for
  people. The report's `damage` and `warnings` are `{"code", "message"}`
  too.
- Times are exact: `{"exact": "1001/30000", "seconds": 0.0333...}`, where
  `exact` is the time and `seconds` only for display. Frame rates and time
  bases are `"30000/1001"`, or `"25"`.
- frame's `result` says which frame it rendered (`pts` and its time,
  with its number where it is known), what was asked for, the view, how
  it was decoded (and why it fell back, if it did), the GPU, the colour
  it decoded with and what of it was assumed, and the source stream; a
  failed run has no `result`. Its `warnings` say what it did with the
  file's spherical metadata.
- Exit statuses: 0 when the run did what was asked, 1 when an input or a
  check failed, 2 for a command line that doesn't parse (also a document
  under `--json`), 3 when `frame` lacks a codec or device it needs, and
  4 when `frame` was interrupted (code `cancelled`). A panic is an
  `internal` error, exit 1; an empty stdout means a signal ended the
  run: Ctrl+C for probe and doctor, a second Ctrl+C for frame, or a
  crash.

### Logging

Logs go to stderr. The default filter is `warn,ffmpeg=off`: Spherecut's
warnings, and none of FFmpeg's own messages, since doctor's device checks
make FFmpeg log errors that are expected. `SPHERECUT_LOG` adds `tracing`
EnvFilter directives to that default:

- `SPHERECUT_LOG=spherecut_media=debug` shows Spherecut's debug events,
  still without FFmpeg's messages.
- FFmpeg's messages appear, under the target `ffmpeg`, when the value names
  that target (`ffmpeg=warn`, `debug,ffmpeg=debug`) or when its last global
  level is more verbose than `warn` (`info`, `debug`, `trace`). `warn` and
  `error` keep them off, and `off` turns off everything. FFmpeg's verbose
  level shows as debug, its debug and trace levels as trace. Each event is
  one whole line of FFmpeg's log, starting with the context that logged
  it, such as `[hevc @ 0x...]`.
- A value that doesn't parse is reported (`warning: ignoring
  SPHERECUT_LOG=...`) and the default applies.

The filter doesn't change probe's reports: their `ffmpeg` lines list
FFmpeg's warnings and errors about each file whatever it is set to.

## Development

```sh
cargo xtask check        # rustfmt, clippy -D warnings, Tier 1 tests, doc tests, cargo-deny, cargo-audit
cargo xtask check --fix  # same, applying rustfmt
cargo xtask gen-media    # synthetic test clips into test-media/ (--quick skips the 4K clip, --out <dir>)
cargo xtask hw-test      # Tier 2 run on real hardware; report in hw-reports/
cargo xtask hw-test --list   # the Tier 2 tests it would run
cargo xtask compat PREFIX... [--test PREFIX] [--clean]   # clippy and a link against other FFmpeg builds, Tier 1 against one
```

`cargo xtask compat` takes FFmpeg install prefixes, such as those
`scripts/build-ffmpeg.sh TAG DIR` builds from a release tag, or the
colon-separated list in `SPHERECUT_FFMPEG_COMPAT`, which `cargo xtask
check` also runs it on (DECISIONS D2). It builds against each in a target
directory of its own, `spherecut-compat-<label>`, in the directory
`SPHERECUT_FFMPEG_COMPAT_TARGET_DIR` names or else in `compat/` in
cargo's target directory, and deletes only directories it made. A
build passes only if every FFmpeg library it bound came from the
prefix, checked before the build and after it.

`cargo xtask check` needs `cargo install --locked cargo-deny cargo-audit`
once (or `--no-supply-chain` to skip those two), strace, and ALSA's
development files: it stops first if `strace -V` fails or pkg-config
finds no `alsa`, with the command that installs it (`sudo dnf install
strace alsa-lib-devel`, `sudo pacman -S strace alsa-lib` or `sudo apt
install strace libasound2-dev`).

Tests come in two tiers. Tier 1 runs anywhere, including machines without
a GPU. It needs the FFmpeg codecs listed above and Mesa's lavapipe Vulkan
driver (`mesa-vulkan-drivers` on Fedora and Ubuntu, `vulkan-swrast` on
Arch): `crates/cli/tests/doctor.rs` runs doctor with the Vulkan loader
limited to lavapipe (`VK_DRIVER_FILES`, `VK_ICD_FILENAMES`), and
`crates/media/tests/hw_refusal.rs` checks that the decoder turns streams
down on lavapipe's Vulkan device instead of decoding them in software.
`crates/cli/tests/confinement.rs` and `crates/cli/tests/doctor.rs` run
probe, doctor and frame under `strace -f` and fail on any `AF_INET` or
`AF_INET6` socket, or on probe opening a file a playlist lists; without
strace they fail rather than skip. `crates/cli/tests/frame.rs` renders
on lavapipe, with its own golden images in `crates/cli/tests/goldens/`
(`SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test frame
pipeline_goldens` blesses them, as for render's, DECISIONS D32). The
tests generate the media they need; `gen-media` is not needed first.

Tier 2 needs a real GPU or camera samples; a plain `cargo test` lists
those tests as ignored, and `cargo xtask hw-test` runs them. It needs no
sudo. It records the environment (the power settings and every render
node among it), lists missing prerequisites with the command that
installs them, builds the workspace in release mode, makes the perf clips
the playback benchmarks play, runs every Tier 2 test and benchmark, then
runs Tier 1 on the machine, and writes one report to
`hw-reports/<host>-<commit>.md` (the commit gets `-dirty` when the checkout
has changes outside `hw-reports/`). The perf clips, synth's noisy 5.7K
and 8K HEVC at a camera's bitrate, take x265 a while the first time
(27 minutes for one 5.7K clip on 4 CPUs, the 8K one longer); they are kept in `hw-media/` in cargo's target directory and made
again only when synth's spec of them changes (DECISIONS D37). Options:

- `--samples <dir>`: camera samples (default `samples/` in the checkout; a
  relative path is taken from the current directory). The path reaches the
  tests byte for byte, as `SPHERECUT_BENCH_OUT` (see `docs/perf.md`) does,
  and need not be valid UTF-8.
- `--quick`: a debug build, with the benchmarks, and so the perf clips,
  left out.
- `--no-tier1`: leave out the Tier 1 run on the machine.
- `--list`: print the Tier 2 tests, as the report names them, and stop.
- `--out <file>`: where to write the report.

Tier 2 tests use `/dev/dri/renderD128`, or `SPHERECUT_RENDER_NODE`.
`gui_smoke` opens the viewer on each perf clip and sample, plays it with
the overlay on and judges what it measured; it needs a desktop session
(GNOME or Hyprland, unlocked and on screen), which it finds from a
terminal inside it or from an SSH shell, and skips without one. Its
screenshots go next to the report.
`audio_output_device_plays` plays a quiet 440 Hz tone for about 2 s on
the default output device.
Benchmarks repeat `SPHERECUT_BENCH_RUNS` times (default 3); see
`docs/perf.md`.

`cargo xtask hw-test` exits 1 when a build prerequisite is missing, the
build fails, listing the Tier 2 tests fails or finds none, a Tier 2 test
fails or does not run, a test binary dies, even after its tests
reported, a perf clip can't be made, or a Tier 1 test fails on the
machine. Skips alone exit 0. The report is written either way. Your cargo
settings `term.verbose`, `term.quiet`, `term.color` and
`term.progress.when` (in config or as `CARGO_TERM_*`) and
`RUST_TEST_NOCAPTURE` don't change how it reads cargo's output.

Camera samples go in `samples/<camera>/` (git-ignored); `docs/SAMPLES.md`
lists what is useful.

Design and decisions: `ARCHITECTURE.md`, `DECISIONS.md`. Camera format
notes: `docs/formats/`. Benchmark numbers: `docs/perf.md`.

## License

GPL-3.0-or-later. See `LICENSE`.
