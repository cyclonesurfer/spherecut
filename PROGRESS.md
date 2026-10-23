# Progress

Updated 2026-10-23, at 3d06a2e. Milestone criteria are in `PLAN.md`;
decisions and their reasons in `DECISIONS.md`.

## Status

| Milestone | State |
|---|---|
| M0 Foundation | **Done pending hardware**. The only hardware report (huginn, a9128cf) predates the decoder, doctor and hw-test fixes; a clean rerun at the current commit is still needed. Evidence below |
| M1 Viewer | Next. `VideoDecoder` (software, VA-API, Vulkan) is its first slice |
| M2–M9 | Not started |

## Environments

### Cloud workspace (where the code is written)

| | |
|---|---|
| OS | Ubuntu 24.04.5 LTS, 4 CPUs, 15 GB RAM |
| Rust | 1.97.0 stable (rustup) with rustfmt, clippy, cargo-deny 0.20.2, cargo-audit 0.22.2; 1.88.0 for the MSRV check. clang 18.1.3 |
| FFmpeg | 6.1.1 (Ubuntu), with -dev headers, libx264, libx265, libdav1d, SVT-AV1. Also 7.1, 8.1 and 9.0.2 built from upstream tags, for the compatibility checks in D2 |
| Vulkan | lavapipe only: llvmpipe, Mesa 25.2.8, Vulkan 1.4.318 |
| Devices | no `/dev/dri`, no GPU, so no VA-API and no Vulkan Video |
| Network | crates.io, GitHub and Ubuntu apt reachable through a proxy; ffmpeg.org blocked (recorded at setup) |

Here Tier 1 runs fully. `SPHERECUT_TIER2=1 cargo test --workspace
--no-fail-fast -- --ignored` runs the 9 Tier 2 tests and all 9 skip: 6
with "no DRM render node at /dev/dri/renderD128" and 3 with "samples
directory samples not found". A skip counts as a failed test to cargo, so
the command exits non-zero, and without `--no-fail-fast` cargo stops after
the first test binary. `cargo xtask hw-test --quick` runs them the same
way and reports each one. With `SPHERECUT_SAMPLES` pointing at the
extracted X5 samples, `samples_probe_cleanly` passes.

### Pat's machines

| Machine | Hardware | Software | State |
|---|---|---|---|
| huginn | Ryzen AI Max+ 395, Radeon 8060S (Strix Halo, RDNA 3.5), 128 GB | Fedora 44, kernel 7.0.9, Mesa 26.2.5 (RADV, radeonsi from `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 (Fedora RPM) | `hw-test` at a9128cf, 2026-10-22: 7 passed, 1 skipped (Vulkan Video HEVC). VA-API decode of H.264, HEVC Main and Main 10 matches software; `vainfo` lists HEVC encode. A rerun at the current commit is wanted |
| odin | RX 7600 (RDNA 3) | Fedora | No report yet |
| Framework 13 Pro | Intel Xe3 | Arch (Omarchy), Hyprland | Arrives late 2026 |

Access: none direct from the cloud workspace. Remote Control on odin is
set up for another project's folder, and a CLI session on huginn wasn't
reachable from here, so hardware runs go through a prompt Pat pastes into
a Claude Code session on the machine, which opens a pull request with the
report (PR #1 for huginn).

## Samples

- **Insta360 X5**, firmware v1.1.22 (`probe` can't confirm it until the
  trailer is parsed), 7 takes with LRVs, clips 1–4 and 8–10 of
  `docs/SAMPLES.md`: 8K30, 8K25, 5.7K60 (HDR video mode), 4K120, the
  tripod seam clip, the handheld flips and the slow rotation. All 14 video
  files probe cleanly at 59976e9 and are detected as Insta360 dual fisheye
  or LRV proxy. Findings: `docs/formats/insta360.md`, "Pat's X5 samples".
  Still wanted: I-Log, 360 audio, FlowState on, a Studio export, a split
  recording.
- GoPro and DJI: none yet.

## M0 acceptance criteria

| Criterion (`PLAN.md`) | Evidence |
|---|---|
| Environment recorded | The tables above, and access below |
| Opening questions and sample request | Sent 2026-10-22; the sample list is `docs/SAMPLES.md`. Pat answered with Insta360 X5 samples (above) |
| Workspace | Crates `core`, `sphere`, `media`, `synth`, `testkit`, `cli`, plus `xtask`; all GPL-3.0-or-later. Workspace lints deny `unsafe_op_in_unsafe_fn` and undocumented `unsafe` blocks; `unwrap_used` and `expect_used` warn, allowed only in tests (`clippy.toml`), and clippy runs with `-D warnings` |
| `cargo xtask check` | `xtask/src/check.rs`. `cargo xtask check` passes at 3d06a2e: rustfmt, clippy, the Tier 1 tests, doc tests, `cargo deny` (advisories, bans, licenses, sources) and `cargo audit` |
| `cargo xtask gen-media` | 12 clips (grid, high-frequency and seam patterns; H.264 and HEVC; 8 and 10-bit; 25, 29.97, 30 and 59.94 fps; stereo and ambiX; with and without Spherical V2; a 4K clip; lossless variants) and 6 broken files. `rendering_is_deterministic` in `crates/synth/src/lib.rs`; two full runs on the fix/writers branch were byte-identical (D9) |
| `spherecut probe` | `crates/cli/tests/probe.rs`; `broken_files_give_errors_not_panics` in `crates/synth/tests/generated_media.rs` (every bad file, plus 4000 edits of a real `moov`); the MP4 fuzzing (D15) |
| `spherecut doctor` | `crates/media/tests/doctor.rs`, and `crates/cli/tests/doctor.rs` against lavapipe. Capability checks and fixes: D18 |
| `cargo xtask hw-test` | Unit tests in `xtask/src/hw_test.rs`; the huginn run; the reporting rules in D19 |
| First hardware report | `hw-reports/huginn-a9128cf8df-dirty.md`: toolchain, a Vulkan device (RADV), VA-API HEVC decode working, and the Vulkan Video HEVC gap named |
| Docs | README, PLAN, ARCHITECTURE, DECISIONS, PROGRESS, `docs/PROMPT.md`, `docs/SAMPLES.md`, `docs/formats/{insta360,gopro,dji}.md`, `docs/perf.md` |

The M0 review at a9128cf found two blockers (B1, B2) and six should-fixes
(S1 to S6). They are addressed by the merges fa7d370 (hw-test), 8c664d2
(FFI and decoder), c12a4d4 (doctor), 1a3f9c7 (writers) and 4603a56 (MP4
and probe), and by 9300fc0 and e33b77b.

## Known issues and limits

- `VideoDecoder` copies hardware frames to system memory. Zero-copy
  (VA-API to DMA-BUF to Vulkan) is planned for M1/M8 performance work.
- `VideoDecoder` doesn't detect Matroska cuts or most fragmented-MP4 cuts,
  and `probe` checks Matroska only against its Segment length (D13, D15).
- The decoder's invalid-data skip path has no test.
- Some tests and the encode paths still call ffmpeg-next accessors that
  build C enums (D11).
- The Insta360 trailer is detected but not parsed, so `probe` shows no
  model or firmware.
- GoPro and DJI detection comes from format research only; no samples
  have been checked.
- Fedora's `ffmpeg-free` lacks libx264 and libx265, which `gen-media` and
  the Tier 1 media tests need. Use RPM Fusion's `ffmpeg`. Tier 1 also needs
  libdav1d, an FFmpeg built with Vulkan, and Mesa's lavapipe.
- `hw-test`'s prerequisite check still judges Fedora by installed packages
  (`ffmpeg-free`, `mesa-va-drivers-freeworld`), unlike doctor (D18).
- `hw-test`'s `vainfo` and prerequisite checks look only at
  `/dev/dri/renderD128`.
- Doctor doesn't print the full VA-API profile list; the hw-test report
  includes `vainfo`.
- ambiX AAC plays as 4.0 surround in players that ignore `SA3D` (D17).
- huginn's numbers are single runs from before the M0 fixes (`docs/perf.md`).
- No GUI yet, so no GUI smoke run (M1).

## Next

1. Rerun `cargo xtask hw-test` on huginn at the current commit, then the
   first run on odin.
2. M1: the `MediaBackend` trait around `VideoDecoder` (seek, audio, worker
   threads, frame cache), then the wgpu projection shader and `spherecut
   frame`, golden images on lavapipe, then the eframe GUI. Bump
   `rust-version` to 1.95 with the GUI crates (D1).
3. The Insta360 trailer reader (metadata, calibration, IMU), needed from M2.

## How to run

```sh
cargo xtask check                 # everything Tier 1 (add --fix to apply rustfmt)
cargo xtask gen-media             # synthetic media into test-media/
cargo run -p spherecut-cli -- probe <file>... [--json]
cargo run -p spherecut-cli -- doctor [--json]
cargo xtask hw-test               # on a real machine; report in hw-reports/
```
