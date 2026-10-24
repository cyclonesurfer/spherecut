# Spherecut

A native Linux editor for 360° video, written in Rust, in early
development. It is being built to open raw files from Insta360, GoPro and
DJI 360 cameras as well as stitched equirectangular video, to reframe them
into flat video or edit them as 360, and to export with the metadata
YouTube and headsets need. It will render with wgpu on Vulkan. It decodes
with FFmpeg, in software or on the GPU through VA-API.

It runs offline: no accounts, no telemetry, no network access.

**Status: early.** Milestone M0 (foundations) is done; M1, the viewer, is
in progress, so there is no GUI yet.
What works today is the `spherecut` command line tool:

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
- Rust 1.88 or newer (1.95 once the GUI lands), with rustfmt and clippy for
  development.
- clang, for generating the FFmpeg bindings at build time.
- For the Tier 1 tests: Mesa's lavapipe Vulkan driver and strace (see
  Development).

### Fedora (43 or newer)

Fedora's own FFmpeg (`ffmpeg-free`) and Mesa VA-API driver leave out H.264
and HEVC, which nearly every 360 camera records. Use RPM Fusion's builds
(enable the RPM Fusion free repository first; see rpmfusion.org):

```sh
sudo dnf swap ffmpeg-free ffmpeg --allowerasing  # or, without ffmpeg-free: sudo dnf install ffmpeg --allowerasing
sudo dnf install ffmpeg-devel clang-devel pkgconf-pkg-config libva-utils vulkan-tools mesa-vulkan-drivers
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
sudo pacman -S --needed rust ffmpeg clang pkgconf libva-utils vulkan-tools
sudo pacman -S --needed vulkan-radeon                       # AMD
sudo pacman -S --needed vulkan-intel intel-media-driver     # Intel
sudo pacman -S --needed vulkan-swrast strace                # lavapipe and strace, for the Tier 1 tests
```

### Ubuntu 24.04 (development only)

```sh
sudo apt install pkg-config clang ffmpeg libavcodec-dev libavformat-dev \
  libavutil-dev libswscale-dev libswresample-dev mesa-vulkan-drivers vulkan-tools vainfo \
  strace
```

Your user needs to be in the `render` group for GPU video
(`sudo usermod -aG render "$USER"`, then log in again). `spherecut doctor`
checks the FFmpeg codecs, the GPU drivers and access to the render node.

## Build and run

```sh
cargo build --release
./target/release/spherecut doctor
./target/release/spherecut probe VID_20261022_165451_00_004.insv
./target/release/spherecut probe --json clip.mp4
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

`spherecut doctor [--render-node /dev/dri/renderD128] [--json]` exits 1
only when a problem is an error: FFmpeg has no H.264, HEVC, ProRes or AAC
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

### JSON output

`spherecut probe --json` and `spherecut doctor --json` print exactly one
line of JSON on stdout per run, on success and on failure, and keep
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
- Exit statuses: 0 when the run did what was asked, 1 when an input or a
  check failed, 2 for a command line that doesn't parse (also a document
  under `--json`); 3 (a missing codec or device) and 4 (interrupted) are
  reserved for `spherecut frame` and the GUI. A panic is an `internal`
  error, exit 1; an empty stdout means the run crashed.

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
```

`cargo xtask check` needs `cargo install --locked cargo-deny cargo-audit`
once (or `--no-supply-chain` to skip those two), and strace: it stops
first if `strace -V` fails, with the command that installs it
(`sudo dnf install strace`, `sudo pacman -S strace` or
`sudo apt install strace`).

Tests come in two tiers. Tier 1 runs anywhere, including machines without
a GPU. It needs the FFmpeg codecs listed above and Mesa's lavapipe Vulkan
driver (`mesa-vulkan-drivers` on Fedora and Ubuntu, `vulkan-swrast` on
Arch): `crates/cli/tests/doctor.rs` runs doctor with the Vulkan loader
limited to lavapipe (`VK_DRIVER_FILES`, `VK_ICD_FILENAMES`), and
`crates/media/tests/hw_refusal.rs` checks that the decoder turns streams
down on lavapipe's Vulkan device instead of decoding them in software.
`crates/cli/tests/confinement.rs` and `crates/cli/tests/doctor.rs` run
probe and doctor under `strace -f` and fail on any `AF_INET` or
`AF_INET6` socket, or on probe opening a file a playlist lists; without
strace they fail rather than skip. The tests generate the media they
need; `gen-media` is not needed first.

Tier 2 needs a real GPU or camera samples; a plain `cargo test` lists
those tests as ignored, and `cargo xtask hw-test` runs them. It needs no
sudo. It records the environment, lists missing prerequisites with the
command that installs them, builds the workspace in release mode, runs
every Tier 2 test and benchmark, and writes one report to
`hw-reports/<host>-<commit>.md` (the commit gets `-dirty` when the checkout
has changes outside `hw-reports/`). Options:

- `--samples <dir>`: camera samples (default `samples/` in the checkout; a
  relative path is taken from the current directory). The path reaches the
  tests byte for byte, as `SPHERECUT_BENCH_OUT` (see `docs/perf.md`) does,
  and need not be valid UTF-8.
- `--quick`: a debug build, with the benchmarks left out.
- `--out <file>`: where to write the report.

Tier 2 tests use `/dev/dri/renderD128`, or `SPHERECUT_RENDER_NODE`.
Benchmarks repeat `SPHERECUT_BENCH_RUNS` times (default 3); see
`docs/perf.md`.

`cargo xtask hw-test` exits 1 when a build prerequisite is missing, the
build fails, listing the Tier 2 tests fails or finds none, a Tier 2 test
fails or does not run, or a test binary dies, even after its tests
reported. Skips alone exit 0. The report is written either way. Your cargo
settings `term.verbose`, `term.quiet`, `term.color` and
`term.progress.when` (in config or as `CARGO_TERM_*`) and
`RUST_TEST_NOCAPTURE` don't change how it reads cargo's output.

Camera samples go in `samples/<camera>/` (git-ignored); `docs/SAMPLES.md`
lists what is useful.

Design and decisions: `ARCHITECTURE.md`, `DECISIONS.md`. Camera format
notes: `docs/formats/`. Benchmark numbers: `docs/perf.md`.

## License

GPL-3.0-or-later. See `LICENSE`.
