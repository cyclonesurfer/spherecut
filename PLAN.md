# Spherecut plan

The milestones from `docs/PROMPT.md`, refined into acceptance criteria. Each
milestone leaves the app working end to end. "Tier 1" tests run anywhere
(headless, lavapipe, software codecs, synthetic media); "Tier 2" tests need
real hardware or Pat's samples and run through `cargo xtask hw-test`.

A milestone is **done** when every criterion below is met, the Tier 1 suite,
clippy, rustfmt and cargo-deny/audit pass, a fresh reviewer has checked it
against this list, and (for GPU/codec/GUI work) a hardware report from at
least one AMD machine is clean. Until the hardware report is back it is
**done pending hardware** in PROGRESS.md.

Order is as in the prompt. If a milestone is blocked on samples or hardware,
the next unblocked one starts and the blocked one resumes when unblocked.

---

## M0 Foundation

- [ ] Environment recorded in PROGRESS.md (toolchain, Vulkan/lavapipe, FFmpeg
      version and headers, network reach, device access).
- [ ] Cargo workspace with the crates that have real content so far; every
      crate GPL-3.0-or-later, `#![deny(unsafe_op_in_unsafe_fn)]`, no
      `unwrap`/`expect` outside tests.
- [ ] `cargo xtask check` runs rustfmt, clippy `-D warnings`, Tier 1 tests,
      cargo-deny and cargo-audit, and fails on any of them.
- [ ] `cargo xtask gen-media` writes deterministic synthetic test media to
      `test-media/`: equirect test patterns (lat/long grid with labelled
      directions, a high-frequency pattern for aliasing tests, a seam
      pattern), in H.264 and HEVC, 8-bit and 10-bit, at several frame rates
      including 29.97 and 59.94, with stereo and 4-channel ambiX audio, with
      and without Spherical Video V2 metadata, plus truncated and corrupt
      variants. Re-running produces byte-identical pattern frames.
- [ ] `spherecut probe <file>` prints container, streams, codecs, pixel
      format, color (matrix/primaries/transfer/range), frame rate and
      timebase as exact rationals, duration, spherical metadata (st3d/sv3d
      from the file's own boxes), ambisonic metadata (SA3D), and detected
      camera/format. `--json` gives the same as machine-readable JSON.
      Truncated, corrupt and non-media files give a clear error and a
      non-zero exit, never a panic (tested on the generated bad files).
- [ ] `spherecut doctor` reports which decoders, encoders and hardware
      paths (VA-API devices and profiles, Vulkan devices) are available and,
      for anything missing, the exact `dnf`/`pacman` command that fixes it.
      The GUI will run the same check at startup.
- [ ] `cargo xtask hw-test` runs without sudo from a fresh clone: checks
      prerequisites (printing the install command for anything missing),
      records the environment (host, kernel, Mesa, FFmpeg, GPU,
      `vulkaninfo --summary`, `vainfo`, FFmpeg hwaccels/encoders), builds in
      release mode, runs Tier 2 tests and benchmarks, and writes one
      self-contained report to `hw-reports/<host>-<commit>.md`. Tier 2 tests
      skip with a stated reason under plain `cargo test` and never pass
      silently without hardware.
- [ ] First hardware report back from odin or huginn showing the toolchain,
      a Vulkan device and VA-API HEVC decode working (or exactly what's
      missing).
- [ ] Docs: README (what it is, how to build and run), PLAN, ARCHITECTURE,
      DECISIONS, PROGRESS, `docs/PROMPT.md`, `docs/SAMPLES.md`,
      `docs/formats/<camera>.md` research notes, `docs/perf.md` skeleton.

## M1 Viewer

- [ ] `MediaBackend` trait with the FFmpeg implementation: open, stream
      info, seek (frame-accurate, by PTS), decode to NV12/P010 frames, both
      software and VA-API (with automatic software fallback), audio decode.
- [ ] Projection shader (WGSL) sampling NV12/P010 planes directly, with
      per-source YUV→RGB matrix and range, into a linear-light float target,
      output as sRGB/Rec.709. Equirect source; rectilinear, fisheye and
      stereographic output with morph.
- [ ] No seam line at ±180° and no shimmer in wide-FOV/tiny-planet views:
      analytic LOD (explicit gradients) plus mipmaps or supersampling,
      proven by golden images of the high-frequency pattern.
- [ ] `spherecut frame <file> --time <t> --yaw/--pitch/--roll --fov
      --projection --size WxH -o out.png` renders one frame headless.
- [ ] Golden-image tests on lavapipe: each projection, the seam, the poles,
      10-bit input, limited vs full range, compared by PSNR/SSIM with stored
      small references. GPU output matches the CPU reference projection in
      `spherecut-sphere` within tolerance.
- [ ] GUI (eframe + wgpu + dock): open a file, viewer panel with a custom
      wgpu paint callback, drag-to-grab-the-sphere (the point under the
      cursor stays under it, tested numerically), scroll/pinch FOV zoom,
      projection switch, play/pause, scrubbing, frame step, audio playback
      with the audio clock driving A/V sync.
- [ ] Decode, audio and texture upload on workers; UI never blocks on them
      (asserted by a test harness that injects a slow decoder). Bounded frame
      cache sized from available memory, configurable.
- [ ] Performance overlay: decode ms, upload ms, render ms, dropped frames,
      cache size.
- [ ] Startup codec/hardware check in the GUI with the doctor's fix
      instructions.
- [ ] HiDPI and fractional scaling correct; works under GNOME and Hyprland
      (hardware report + screenshots).
- [ ] Hardware: 5.7K30 stitched equirect plays in real time on odin and
      huginn with VA-API decode; numbers in `docs/perf.md`.

## M2 Clip reframer

The first version Pat can use for real.

- [ ] Single-clip reframer mode on the core data model (a sequence with one
      clip), so M5's timeline extends it rather than replacing it.
- [ ] Trim in/out with exact rational times; markers.
- [ ] Keyframes for orientation (quaternion), FOV, projection and morph;
      set from the viewer; interpolation on SO(3) (squad or a C¹ spline)
      with easing presets; property tests for continuity and no flips
      through ±180°.
- [ ] Curve editor panel (FOV, morph, and yaw/pitch/roll as displayed
      views of the quaternion track) with easing presets.
- [ ] Look-at target keyframes.
- [ ] Full-sphere overview showing where the virtual camera points.
- [ ] Project save/load (format chosen and recorded in DECISIONS.md) with a
      schema version and relative media paths; undo/redo for every edit.
- [ ] Flat export: 16:9, 9:16, 1:1, 4:5 at 1080p/1440p/4K, project frame
      rate, H.264/HEVC/AV1 via VA-API with libx264/libx265/SVT-AV1
      fallback; audio passed through or re-encoded.
- [ ] 360 export: equirect up to 7680×3840 with st3d/sv3d (and SA3D for
      ambiX), written by our own box writer and validated by re-parsing.
- [ ] Export runs in the background with progress and cancel; the UI stays
      responsive.
- [ ] Parity tests: decode exported frames and compare against
      `spherecut frame` renders of the same times (PSNR threshold), flat and
      360.
- [ ] `spherecut render <project> -o out.mp4 [--preset]` headless.
- [ ] Synthetic motion blur for fast pans (sub-frame sampling), off by
      default, with a golden test.
- [ ] Hardware: reframed 4K30 exports at or above real time with VA-API on
      odin and huginn.

## M3 Raw ingest and stitching

Main camera first (Pat to confirm), then the others.

- [ ] Format notes in `docs/formats/<camera>.md` verified against Pat's
      samples, per model and firmware.
- [ ] Container and metadata parsing (vendor trailer / GPMF / DJI metadata)
      via telemetry-parser where it fits; frame-locked decode of lens pairs.
- [ ] Source models in the shader: dual fisheye with calibration, GoPro
      EAC pair; feathered seam blending; per-lens exposure/white-balance
      match; stitch-distance control.
- [ ] Manual fine-tune panel with saved per-camera profiles.
- [ ] Synthetic round trip: render a known equirect scene through the
      camera model, stitch back, compare (Tier 1).
- [ ] .lrv/.LRV companions used as instant proxies.
- [ ] DJI: raw if feasible, otherwise stitched export plus a written
      finding.
- [ ] Nadir/zenith cleanup and optional nadir patch/logo.

## M4 Stabilization and horizon

- [ ] Gyro/accel ingest, IMU-to-frame sync with automatic offset estimation.
- [ ] Orientation integration with adaptive gravity fusion (gyro-only when
      |a| is far from 1 g); horizon holds through pitch past ±90° and flips.
- [ ] Modes: lock, follow heading, free; per-clip smoothing.
- [ ] Ambisonic audio rotated by the same orientation.
- [ ] Synthetic round trip with known rotations including flips (Tier 1).

## M5 Timeline

- [ ] Multi-track video/audio/title tracks; insert/overwrite, ripple, roll,
      slip, slide, razor, snapping, markers, J/L cuts, nested sequences.
- [ ] Projects with migrations, relinking, autosave and crash recovery;
      undo for every edit.
- [ ] Media bin with thumbnails, metadata, camera detection, search; probe
      panel.
- [ ] Proxies: .lrv instantly, otherwise background short-GOP hardware
      proxies; per-viewer toggle; export always from originals.
- [ ] Speed: constant and ramped (time-remap curves), frame blending, audio
      stretch with pitch preserved or mute.
- [ ] Transitions: crossfade, dip to color, spin, whip pan, tiny-planet zoom.
- [ ] JKL shuttle and remappable standard NLE shortcuts.

## M6 Audio

- [ ] Mixer: clip/track gain, fades, keyframed volume, pan, music beds.
- [ ] EBU R128 loudness normalization to platform targets.
- [ ] ambiX end to end: binaural/stereo decode steered by the virtual camera
      for flat exports; rotation and SA3D passthrough for 360 exports.
- [ ] Wind-noise reduction and high-pass filter.

## M7 Color and effects

- [ ] Per-source matrix/primaries/transfer/range; 10-bit throughout; linear
      float working space; Rec.709 SDR output.
- [ ] .cube 3D LUTs; exposure, white balance, contrast, saturation,
      lift/gamma/gain, curves.
- [ ] GPU waveform, vectorscope, histogram.
- [ ] Titles/overlays, screen-locked (flat) and world-locked (360).
- [ ] Keyframable blur, sharpen, vignette.

## M8 Delivery

- [ ] Presets: YouTube 360 8K/5.7K/4K, YouTube 16:9, Instagram/TikTok 9:16,
      archive (10-bit high bitrate).
- [ ] Render queue: background, progress, cancel, multiple jobs.
- [ ] Export validation of every output (metadata re-parse).
- [ ] Performance pass against the targets, numbers per machine.
- [ ] Install script and .desktop file; then RPM spec and PKGBUILD.

## M9 Advanced

Optical-flow seams and frame interpolation, subject tracking driving the
view, rolling-shutter correction, multicam sync by audio, zero-copy
decode-to-render (dmabuf/Vulkan interop) if profiling shows it matters.
