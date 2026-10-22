# Spherecut architecture

This describes the design as built, plus the planned parts marked
*(planned)*. DECISIONS.md records why each choice was made.

## Crates

```
                    app (egui GUI)   cli (`spherecut` binary)   tests
                          \                |                    /
                           +------ export, audio (planned) ----+
                                      |
                             render (wgpu, WGSL)  (planned)
                                      |
             motion (IMU, stabilization)   stitch (lens models, blending)  (planned)
                                      |
                       camera (insta360, gopro, dji)  (planned)
                                      |
                              media (MediaBackend, FFmpeg, probing, MP4 boxes)
                                      |
                 sphere (projection math)      core (time, project model, undo)
```

| Crate | Role | Status |
|---|---|---|
| `spherecut-core` | Product name constants, exact rational time; later the project model, timeline, keyframes, undo | started |
| `spherecut-sphere` | CPU reference projection math in f64: equirect, cube/EAC, fisheye lens models, view projections and morphs, rotation helpers | started |
| `spherecut-media` | `MediaBackend` trait, FFmpeg backend in a safe RAII layer, probing, our own ISO-BMFF box reader/writer for spherical metadata, hardware capability detection | M0 |
| `spherecut-camera` | Per-camera raw formats: file pairing, vendor metadata, calibration | M3 |
| `spherecut-stitch` | Lens calibration models, seam blending, exposure matching | M3 |
| `spherecut-motion` | IMU ingest, time sync, orientation fusion, stabilization | M4 |
| `spherecut-render` | wgpu device setup, render graph, projection/color shaders, scopes, golden-image harness | M1 |
| `spherecut-audio` | Playback clock, mixing, ambisonics, loudness | M1 (playback), M6 |
| `spherecut-export` | Encode pipeline, render queue, metadata injection and validation | M2 |
| `spherecut-app` | eframe app, docked panels, viewer paint callbacks | M1 |
| `spherecut-cli` | The `spherecut` binary: `probe`, `doctor`, `frame`, `render`, and the GUI when run without a subcommand | M0 |
| `xtask` | `check`, `gen-media`, `hw-test` | M0 |

Crates are created when they get real content, not as empty shells.

## Coordinates and conventions

* World and camera space are right-handed: **+X right, +Y up, −Z forward**.
  The centre of an equirect frame is forward.
* Longitude `atan2(x, −z)`, latitude `asin(y)`. Equirect `u = 0.5 + lon/2π`,
  `v = 0.5 − lat/π`, `v = 0` at the top.
* Normalised image coordinates `(u, v) ∈ [0, 1]²`; texel centres at
  half-integers.
* Stored rotations are unit quaternions (`glam::DQuat` on the CPU). A view
  orientation `q` maps view-space rays to world space: `world = q * view`.
  The UI may show yaw/pitch/roll, derived on display.
* A lens or virtual camera looks down its own −Z axis.

## Time

All stored time is exact (`spherecut_core::time`):

* `Rational`: reduced `i64/i64`, checked arithmetic in `i128`.
* `Time`: seconds as a `Rational`; timeline positions are `Time`.
* `FrameRate`: positive `Rational` (`30000/1001` for 29.97).
* Streams keep integer PTS in their own timebase; conversion to and from
  `Time` is explicit about rounding. Variable-frame-rate sources are
  addressed by PTS, never by frame index.

## Rendering *(planned, M1)*

Every source type implements, in WGSL, "direction on the unit sphere →
texel(s)": equirect, the GoPro EAC pair, dual fisheye with calibration. For
each output pixel the shader:

1. builds a ray from the output projection (rectilinear / fisheye /
   stereographic with morph, `sphere::perspective` is the reference),
2. rotates it by the keyframed view orientation and the stabilization
   rotation,
3. maps it into each contributing source and samples the **YUV planes
   directly** (NV12: R8 + RG8, P010: R16 + RG16), converting with the
   source's matrix and range,
4. blends lenses across the seam where relevant, converts to linear light
   and runs the color pipeline in float, then writes the output.

Stitching, stabilization and reframing therefore happen in one pass, shared
by the viewer and the exporter. An intermediate equirect exists only as the
output of a 360 export.

The ±180° seam and the poles break hardware derivative-based LOD, so the
shader computes LOD analytically from the output pixel's angular footprint
and samples with `textureSampleGrad`/`textureSampleLevel`.

## Threads *(planned, M1)*

* UI thread: egui, never blocks on decode, disk or GPU readback.
* Decode workers per open source, feeding a bounded frame cache.
* Audio thread (cpal): owns the playback clock; video presentation follows
  it.
* Background workers for thumbnails, waveforms, proxies and export, each
  with progress reporting and cancellation tokens.

## Testing tiers

* **Tier 1** runs anywhere: unit and property tests, golden images on
  lavapipe, software decode/encode, synthetic media from `cargo xtask
  gen-media`. `cargo xtask check` runs all of it.
* **Tier 2** needs hardware or samples. Tier 2 tests are `#[ignore]`d with a
  reason and run by `cargo xtask hw-test`, which also refuses to report a
  pass for a test whose prerequisite (device, sample) is missing; such tests
  are reported as *skipped* with the reason.
