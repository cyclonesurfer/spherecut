# Spherecut

A native Linux editor for 360° video, written in Rust. It opens raw files
from Insta360, GoPro and DJI 360 cameras as well as stitched equirectangular
video, lets you reframe them into flat video or edit them as 360, and
exports with the metadata YouTube and headsets need. It renders with wgpu on
Vulkan and decodes with FFmpeg, on the GPU through VA-API where it can.

It runs offline: no accounts, no telemetry, no network access.

**Status: early.** Milestone M0 (foundations) is done; there is no GUI yet.
What works today is the `spherecut` command line tool:

- `spherecut probe <file>...` shows what a file is: container, streams,
  codecs, colour, exact frame rates and timebases, spherical video and
  ambisonic metadata, and which camera and layout it came from. `--json`
  gives the same as JSON.
- `spherecut doctor` checks which decoders, encoders and GPU video paths
  your system has, and prints the exact `dnf` or `pacman` command for
  anything missing.

See `PLAN.md` for the milestones and `PROGRESS.md` for where things stand.

## Requirements

- Linux with an AMD or Intel GPU (Mesa RADV or ANV). NVIDIA isn't supported.
- FFmpeg 6.1 or newer, with its development files, H.264/HEVC decoders, and
  libx264/libx265.
- Rust 1.88 or newer (1.95 once the GUI lands), with rustfmt and clippy for
  development.
- clang, for generating the FFmpeg bindings at build time.

### Fedora (43 or newer)

Fedora's own FFmpeg (`ffmpeg-free`) and Mesa VA-API driver leave out H.264
and HEVC, which nearly every 360 camera records. Use RPM Fusion's builds
(enable the RPM Fusion free repository first; see rpmfusion.org):

```sh
sudo dnf swap ffmpeg-free ffmpeg --allowerasing
sudo dnf install ffmpeg-devel clang-devel pkgconf-pkg-config libva-utils vulkan-tools
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
```

### Ubuntu 24.04 (development only)

```sh
sudo apt install pkg-config clang ffmpeg libavcodec-dev libavformat-dev \
  libavutil-dev libswscale-dev libswresample-dev mesa-vulkan-drivers vulkan-tools vainfo
```

Your user needs to be in the `render` group for GPU video
(`sudo usermod -aG render "$USER"`, then log in again). `spherecut doctor`
checks all of this.

## Build and run

```sh
cargo build --release
./target/release/spherecut doctor
./target/release/spherecut probe VID_20261022_165451_00_004.insv
./target/release/spherecut probe --json clip.mp4
```

Logging goes to stderr; set `SPHERECUT_LOG=debug` for more.

## Development

```sh
cargo xtask check        # rustfmt, clippy -D warnings, tests, cargo-deny, cargo-audit
cargo xtask check --fix  # same, applying rustfmt
cargo xtask gen-media    # synthetic test clips into test-media/
cargo xtask hw-test      # Tier 2 run on real hardware; report in hw-reports/
```

`cargo xtask check` needs `cargo install --locked cargo-deny cargo-audit`
once (or `--no-supply-chain` to skip those two).

Tests come in two tiers. Tier 1 runs anywhere, including machines without
a GPU. Tier 2 needs a real GPU or camera samples; a plain `cargo test` lists
those tests as ignored, and `cargo xtask hw-test` runs them and writes one
report with the results, benchmarks and environment. Camera samples go in
`samples/<camera>/` (git-ignored); `docs/SAMPLES.md` lists what is useful.

Design and decisions: `ARCHITECTURE.md`, `DECISIONS.md`. Camera format
notes: `docs/formats/`.

## License

GPL-3.0-or-later. See `LICENSE`.
