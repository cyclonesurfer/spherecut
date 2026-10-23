# Spherecut

A native Linux editor for 360° video, written in Rust, in early
development. It is being built to open raw files from Insta360, GoPro and
DJI 360 cameras as well as stitched equirectangular video, to reframe them
into flat video or edit them as 360, and to export with the metadata
YouTube and headsets need. It will render with wgpu on Vulkan. It decodes
with FFmpeg, in software or on the GPU through VA-API.

It runs offline: no accounts, no telemetry, no network access.

**Status: early.** Milestone M0 (foundations) is done pending a hardware
rerun; there is no GUI yet.
What works today is the `spherecut` command line tool:

- `spherecut probe <file>...` shows what a file is: container, streams,
  codecs, colour, exact frame rates and timebases, spherical video and
  ambisonic metadata, and the camera vendor and layout it most likely came
  from. It also checks MP4/MOV files for truncation and corruption.
  `--json` gives the same as JSON.
- `spherecut doctor` checks what your FFmpeg, drivers and GPU can actually
  do. It decodes one small frame each of H.264 High, HEVC Main, HEVC Main 10
  and AV1 in software, on VA-API and on FFmpeg's Vulkan device, encodes one
  frame with each VA-API encoder, and lists the Vulkan devices. For each
  problem it prints a fix: the exact `dnf` or `pacman` command where a
  package fixes it.

See `PLAN.md` for the milestones and `PROGRESS.md` for where things stand.

## Requirements

- Linux with an AMD or Intel GPU (Mesa RADV or ANV). NVIDIA isn't supported.
- FFmpeg 6.1 or newer, with its development files, H.264/HEVC decoders,
  an AV1 software decoder (libdav1d), and libx264/libx265, built with
  Vulkan support (the Tier 1 doctor test needs FFmpeg's Vulkan device).
- Rust 1.88 or newer (1.95 once the GUI lands), with rustfmt and clippy for
  development.
- clang, for generating the FFmpeg bindings at build time.
- For the Tier 1 tests: Mesa's lavapipe Vulkan driver (see Development).

### Fedora (43 or newer)

Fedora's own FFmpeg (`ffmpeg-free`) and Mesa VA-API driver leave out H.264
and HEVC, which nearly every 360 camera records. Use RPM Fusion's builds
(enable the RPM Fusion free repository first; see rpmfusion.org):

```sh
sudo dnf swap ffmpeg-free ffmpeg --allowerasing  # or, without ffmpeg-free: sudo dnf install ffmpeg --allowerasing
sudo dnf install ffmpeg-devel clang-devel pkgconf-pkg-config libva-utils vulkan-tools mesa-vulkan-drivers
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
sudo pacman -S --needed vulkan-swrast                       # lavapipe, for the Tier 1 tests
```

### Ubuntu 24.04 (development only)

```sh
sudo apt install pkg-config clang ffmpeg libavcodec-dev libavformat-dev \
  libavutil-dev libswscale-dev libswresample-dev mesa-vulkan-drivers vulkan-tools vainfo
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

`spherecut probe` exits 0 when every file is healthy, and 1 when any file
can't be opened, is truncated or corrupt, or has no video or audio stream.
A damaged file that FFmpeg can open still gets its report, with a
`DAMAGE` line per problem, and stderr gets one line per bad file, such as
`error: clip.mp4 is damaged: <first reason> (and 2 more problems)`. The
`mp4` line names a top-level box cut off by the end of the file:
``truncated `moov` (4847 of 4947 bytes)``. Only MP4/MOV structure is
checked in full; Matroska files are checked against their Segment length
only, and other containers not at all, which a `structure` line says.
`--json` prints one object per file (an array for several) with `damage`,
`structure` (`{"check": "full" | "length" | "none", "why"}`) and
`mp4.truncated` (`fourcc`, `offset`, `header_len`, `size`, `present`, or
null). A file that can't be opened is `{"path", "error"}`.

`spherecut doctor [--render-node /dev/dri/renderD128] [--json]` exits 1
only when a problem is an error: FFmpeg has no H.264, HEVC, ProRes or AAC
decoder, or nothing decodes AV1. Warnings, such as no GPU or a Vulkan
device that runs on the CPU, exit 0. Doctor never installs or changes
anything; on Fedora it makes one read-only `rpm -q` query to word its
fixes.

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
  level shows as debug, its debug and trace levels as trace.
- A value that doesn't parse is reported (`warning: ignoring
  SPHERECUT_LOG=...`) and the default applies.

## Development

```sh
cargo xtask check        # rustfmt, clippy -D warnings, Tier 1 tests, doc tests, cargo-deny, cargo-audit
cargo xtask check --fix  # same, applying rustfmt
cargo xtask gen-media    # synthetic test clips into test-media/ (--quick skips the 4K clip, --out <dir>)
cargo xtask hw-test      # Tier 2 run on real hardware; report in hw-reports/
```

`cargo xtask check` needs `cargo install --locked cargo-deny cargo-audit`
once (or `--no-supply-chain` to skip those two).

Tests come in two tiers. Tier 1 runs anywhere, including machines without
a GPU. It needs the FFmpeg codecs listed above and Mesa's lavapipe Vulkan
driver (`mesa-vulkan-drivers` on Fedora and Ubuntu, `vulkan-swrast` on
Arch): `crates/cli/tests/doctor.rs` runs doctor with the Vulkan loader
limited to lavapipe (`VK_DRIVER_FILES`, `VK_ICD_FILENAMES`). The tests
generate the media they need; `gen-media` is not needed first.

Tier 2 needs a real GPU or camera samples; a plain `cargo test` lists
those tests as ignored, and `cargo xtask hw-test` runs them. It needs no
sudo. It records the environment, lists missing prerequisites with the
command that installs them, builds the workspace in release mode, runs
every Tier 2 test and benchmark, and writes one report to
`hw-reports/<host>-<commit>.md` (the commit gets `-dirty` when the checkout
has changes outside `hw-reports/`). Options:

- `--samples <dir>`: camera samples (default `samples/` in the checkout; a
  relative path is taken from the current directory).
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
