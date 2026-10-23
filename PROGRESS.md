# Progress

Updated 2026-10-23. Milestone criteria are in `PLAN.md`; decisions and their
reasons in `DECISIONS.md`.

## Status

| Milestone | State |
|---|---|
| M0 Foundation | **Done pending hardware**: everything except the first hardware report from huginn or odin |
| M1 Viewer | Next. The first decoder slice (`VideoDecoder`, software and VA-API) is in |
| M2–M9 | Not started |

## Environments

### Cloud workspace (where the code is written)

Recorded 2026-10-23.

| | |
|---|---|
| OS | Ubuntu 24.04.5 LTS, 4 CPUs, 15 GB RAM |
| Rust | 1.97.0 stable (rustup), with rustfmt, clippy, cargo-deny 0.20.2, cargo-audit 0.22.2 |
| FFmpeg | 6.1.1 (Ubuntu), with -dev headers, libx264, libx265, SVT-AV1. Also minimal 8.1 and 9.0.2 builds made from upstream tags, used to check that the workspace compiles and the media tests pass on newer FFmpeg |
| Vulkan | lavapipe only: llvmpipe, Mesa 25.2.8, Vulkan 1.4.318 |
| Devices | no `/dev/dri`, no GPU, so no VA-API and no Vulkan Video |
| Network | crates.io, GitHub and Ubuntu apt reachable through a proxy; ffmpeg.org blocked |

So here Tier 1 runs fully and every Tier 2 test reports "skipped: no DRM
render node". Camera samples can be extracted here (see Samples) for the
sample probe test, which needs no GPU.

### Pat's machines

| Machine | Hardware | Software | State |
|---|---|---|---|
| huginn | Ryzen AI Max+ 395, Radeon 8060S (Strix Halo, RDNA 3.5), 128 GB | Fedora 44, kernel 7.0.9, RADV Mesa 26.0.6, FFmpeg 8.1.1 (`ffmpeg-free` + `libavcodec-freeworld`), Rust 1.95 RPM | Surveyed 2026-10-22. **No hardware video decode yet**: Mesa's VA-API driver isn't installed (and Fedora's has H.264/HEVC off), RADV exposes no Vulkan Video. No rustfmt/clippy, no FFmpeg pkg-config files, no clang. Fix commands sent to Pat; waiting |
| odin | RX 7600 (RDNA 3) | Fedora | Not surveyed yet |
| Framework 13 Pro | Intel Xe3 | Arch (Omarchy), Hyprland | Arrives late 2026 |

The fix for huginn, which `spherecut doctor` and `cargo xtask hw-test` also
print:

```sh
sudo dnf swap ffmpeg-free ffmpeg --allowerasing          # RPM Fusion's full FFmpeg
sudo dnf install ffmpeg-devel mesa-va-drivers-freeworld libva-utils clang-devel pkgconf-pkg-config rustup
rustup-init -y                                           # gets rustfmt and clippy
```

## Samples

- **Insta360 X5**, firmware v1.1.22, 7 takes with LRVs (2026-10-23): 8K30,
  8K25, 5.7K60, 4K120 and three longer 8K30 takes. All 14 files probe
  cleanly and are detected as Insta360 dual fisheye / LRV proxy. Findings:
  `docs/formats/insta360.md`, "Pat's X5 samples". Still wanted: I-Log,
  360 audio, FlowState on, a Studio export, a split recording, and which
  take is which test clip.
- GoPro and DJI: none yet.

## Done in M0

- Workspace: `core` (exact time), `sphere` (projection math), `media`
  (probe, MP4 boxes, spherical metadata read/write, doctor, encoder, decoder),
  `synth` (test patterns and media), `testkit` (Tier 2 gating, benchmark
  recording), `cli` (`spherecut probe`, `spherecut doctor`), `xtask`.
- `cargo xtask check`: rustfmt, clippy `-D warnings`, Tier 1 tests, doc tests,
  cargo-deny, cargo-audit.
- `cargo xtask gen-media`: 11 synthetic clips (H.264/HEVC, 8/10-bit,
  25, 29.97, 30 and 59.94 fps, stereo and ambiX, with and without Spherical V2, a 4K
  clip, lossless variants) and 6 broken files, with a manifest.
- `spherecut probe [--json]`: container, streams, exact rationals, colour,
  st3d/sv3d/SA3D and V1 from the boxes, camera detection (Insta360, GoPro,
  DJI, tagged and likely equirect). Broken files give an error, never a
  panic.
- `spherecut doctor [--json]`: decoders, encoders, VA-API and Vulkan devices,
  and the exact `dnf`/`pacman` fix for each problem, including Fedora's
  codec-stripped packages and render-node permissions.
- `cargo xtask hw-test`: environment capture, prerequisite checks with fixes,
  release build, Tier 2 tests, benchmarks, one Markdown report in
  `hw-reports/`.
- Tier 2 tests: VA-API HEVC Main/Main10 and H.264 decode, bit-exact against
  software, with no software fallback; Vulkan Video HEVC (optional);
  synthetic 4K and 5.7K decode benchmarks; every camera sample probes,
  decodes on VA-API exactly as in software, and is benchmarked as a
  multiple of real time per recording mode.

## Next

1. First hardware report from huginn (after the fixes above), then odin.
2. M1: the `MediaBackend` trait around `VideoDecoder` (seek, audio, worker
   threads, frame cache), then the wgpu projection shader and `spherecut
   frame`, golden images on lavapipe, then the eframe GUI. Bump
   `rust-version` to 1.95 with the GUI crates.
3. The Insta360 trailer reader (metadata, calibration, IMU), needed from M2.

## Known issues and limits

- `VideoDecoder` copies hardware frames to system memory. Zero-copy
  (VA-API to DMA-BUF to Vulkan) is planned for M1/M8 performance work.
- The Insta360 trailer is detected but not parsed yet, so `probe` doesn't
  show model or firmware.
- Detection of GoPro and DJI files is from format research only; no
  samples have been checked.
- Fedora's `ffmpeg-free` lacks libx264/libx265, which `gen-media` and the
  Tier 1 media tests need. Use RPM Fusion's `ffmpeg`.

## How to run

```sh
cargo xtask check                 # everything Tier 1 (add --fix to apply rustfmt)
cargo xtask gen-media             # synthetic media into test-media/
cargo run -p spherecut-cli -- probe <file>... [--json]
cargo run -p spherecut-cli -- doctor
cargo xtask hw-test               # on a real machine; report in hw-reports/
```
