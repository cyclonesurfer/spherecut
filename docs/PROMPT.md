# Spherecut: a native Linux 360° video editor in Rust

*Spherecut is a working name. Keep the name defined in one place so it's trivial to change.*

Approach this as a senior engineer with deep experience in Rust, GPU rendering, video codecs, and NLE design.

## The goal

Build a native Linux editor for 360° video, in Rust, that takes raw files straight off my cameras, stitches and stabilizes them, lets me cut a real multi-track timeline, and exports both reframed flat video and full 360 video, fast, on my own AMD and Intel hardware.

Why: none of the camera makers' desktop apps (Insta360 Studio, GoPro Player, DJI's desktop software) run on Linux, and general-purpose NLEs treat 360 as an afterthought. I want one tool that does the whole job natively, and feels as fluid in the viewer and timeline as the best commercial editors.

This is a long, multi-session build with a large scope. Build it in an order where the app is genuinely usable early and gets deeper with every milestone. Every milestone must leave the app working end to end; never trade a working app for breadth.

## Your environment

You may be running in a cloud workspace with no GPU and a network allowlist (crates.io and GitHub reachable, most other sites not; your web tools can still read documentation). Find out what you actually have and record it in PROGRESS.md.

Real-hardware testing is available and is a first-class part of verification, not a fallback. I can run anything you give me on odin and huginn (and the Framework once it arrives) and send back the results. If this session is also linked to my computer, you may be able to run commands there yourself: check, and if you can, run the hardware tests directly and hand me only what needs my eyes. See "Hardware test runs" below.

Work in my GitHub repo `cyclonesurfer/spherecut` (I'll create it empty for you to attach). Commit in small steps and push at the end of each milestone.

## Hard requirements

- **Linux only.** Wayland-first (GNOME on Fedora, Hyprland on Arch); X11 via XWayland is fine. Correct HiDPI and fractional scaling. Behaves under tiling compositors (arbitrary window sizes, no assumed server-side decorations).
- **Rust**, stable toolchain, Cargo workspace.
- **GPU: wgpu on Vulkan** (Mesa RADV and ANV), WGSL shaders, compute where it helps. No NVIDIA-specific code.
- **UI: egui/eframe** on the wgpu backend, dockable panels (egui_dock or similar), custom wgpu paint callbacks for the viewer, scopes, and timeline thumbnails. It's mature, has first-class wgpu interop, and handles Wayland well. If it hits a genuine wall, write up the problem and alternatives and ask me before switching.
- **Codecs: FFmpeg** via Rust bindings for demux, decode, encode, and mux, behind a `MediaBackend` trait so a Vulkan Video backend could replace it later. Evaluate the current bindings (rsmpeg, ffmpeg-next, ffmpeg-the-third, or your own thin bindgen layer) against the FFmpeg versions Fedora and Arch actually ship, pick one, and record why. Hardware decode and encode through VA-API and/or FFmpeg's Vulkan hwaccel, with software fallback always available.
- **No JavaScript, Node, npm, Electron, Tauri, or webviews** anywhere in the build or runtime. I avoid the npm ecosystem because of its supply-chain incidents. Keep the Rust dependency tree lean, run cargo-deny and cargo-audit, and justify every heavy dependency in DECISIONS.md.
- **No network access at runtime**: no telemetry, update checks, or cloud features.
- **License: GPL-3.0-or-later.** That lets you reuse Gyroflow's GPL code; check every dependency's license against it.
- **No closed vendor SDKs** (Insta360's Media SDK, for example). Everything comes from open formats, open-source references, and reverse engineering my own sample files.

## Hardware it has to run well on

- **odin**: Fedora Workstation, Radeon RX 7600 (RDNA3)
- **huginn**: Fedora, Ryzen AI Max+ 395 with Radeon 8060S (RDNA3.5), 128 GB unified memory
- **Framework Laptop 13 Pro**: Core Ultra X7 358H (Intel Xe3 graphics), 32–64 GB, probably Arch (Omarchy) with Hyprland; arriving late 2026

No NVIDIA anywhere. Two of the three are iGPUs sharing system memory, so be deliberate about large buffers.

Fedora gotcha: Fedora's stock Mesa and ffmpeg-free builds leave out H.264/HEVC for patent reasons; hardware HEVC decode needs RPM Fusion's full ffmpeg and its "freeworld" Mesa VA-API drivers (check whether Mesa's Vulkan Video codecs are affected the same way). At startup, detect missing codec or hardware support and tell me exactly what's missing and how to fix it, rather than failing later with a decoder error.

## Inputs (all of these)

1. **Stitched equirectangular video** from any camera or app: MP4/MOV, H.264/HEVC/AV1/ProRes, 8- and 10-bit, up to 8K, any frame rate including 29.97/59.94 and high-frame-rate modes. Read spherical metadata when present; let me tag a file as equirect when it isn't.
2. **Insta360 raw (.insv)**: dual-fisheye video; depending on model and mode, one file or a pair (one per lens) that must stay frame-locked. Vendor metadata with gyro/accel, exposure, and per-lens calibration, which I believe sits in a trailer at the end of the file. Low-res .lrv companion files.
3. **GoPro Max / Max 2 raw (.360)**: two HEVC streams that together carry an equi-angular cubemap (EAC) in GoPro's own layout, decoded in lockstep, plus GPMF telemetry and 360 audio. Low-res .LRV companion files.
4. **DJI Osmo 360 raw**: research the format; I don't know how much is documented. If full raw support turns out to be infeasible, tell me what you found and support DJI's stitched export in the meantime.

The format details above are my best understanding and may be wrong or out of date. Verify everything against my sample files and open-source references, starting with Gyroflow's telemetry-parser (it parses GoPro GPMF and Insta360 metadata, among others) and max2sphere (Paul Bourke's GoPro Max EAC-to-equirect converter, on GitHub). Document findings per camera in `docs/formats/<camera>.md`, including model and firmware differences.

**Samples**: at the start, ask me which camera I shoot most (implement that one first) and tell me exactly which modes to record (resolutions, frame rates, log/flat profiles, 360 audio on/off) so the samples cover the format variants. I'll record short clips in-camera rather than trimming files, since trimming can drop the metadata and gyro trailers, and put them in a git-ignored `samples/<camera>/` directory or a folder on my machine.

## Outputs (both)

- **Reframed flat video**: a keyframed virtual camera (orientation, field of view, projection) rendered at 16:9, 9:16, 1:1, or 4:5 in 1080p, 1440p, or 4K at the project frame rate.
- **360 video**: equirectangular up to 8K (7680×3840) with Spherical Video V2 metadata (st3d/sv3d) and, for first-order ambisonic audio, SA3D, so YouTube and VR headsets recognize it. Validate every export by re-parsing the file and checking the metadata.
- **Codecs and presets**: HEVC and H.264 through VA-API with software fallback (libx265/libx264), AV1 (VA-API or SVT-AV1), and a high-bitrate 10-bit archive option. Presets for YouTube 360 (8K/5.7K/4K), YouTube 16:9, Instagram/TikTok 9:16, and archive. Render queue with background export, progress, and cancel.
- **Export matches preview**: both go through the same render code, and tests enforce it.

## Architecture (strong suggestions; deviate only for good reason, and record why)

- **Spherical sources.** Every source type (equirect, GoPro EAC pair, dual-fisheye with calibration) implements "direction on the unit sphere → texel(s)" in shader code. For each output pixel, the renderer builds a ray from the output projection, maps it through the keyframed view and the stabilization rotation, and samples the source directly. Stitching and reprojection then happen in the same pass that draws the viewer and the export; an intermediate equirect is produced only for 360 export.
- **Sample YUV directly.** Read NV12/P010 planes in the projection shader instead of converting the whole sphere to RGB every frame; a reframed view only touches the region it shows.
- **The equirect seam.** UV derivatives jump at ±180° longitude, which picks the wrong mip level and draws a visible line. Use explicit gradients or analytic LOD. Avoid aliasing at wide FOVs and in tiny-planet views (mips or supersampling), and prove it with a high-frequency test pattern.
- **Rotations are quaternions** everywhere in stored state; interpolate on SO(3) (slerp/squad or a smooth spline) with easing. The UI may display yaw/pitch/roll.
- **Time is exact.** Rational timestamps (per-stream and project timebases), never f64 timeline positions. Handle 29.97/59.94, mixed frame rates in one timeline, and variable-frame-rate sources by PTS.
- **Threads.** The audio clock drives A/V sync. The UI thread never blocks on decode, disk, or GPU readback; decode, proxies, thumbnails/waveforms, and export run on workers with progress and cancellation.
- **One render core, three front ends**: the GUI, a headless CLI (`spherecut probe | frame | render`), and the test suite. The CLI is how you verify most things without a display.
- **Projects**: human-readable (RON or JSON via serde), schema-versioned with migrations, relative media paths with relinking. Undo/redo for every edit. Autosave and crash recovery.
- **Suggested crates**: `core` (project model, timeline, time, keyframes, undo), `sphere` (projection math), `media` (backend trait, FFmpeg, probing, proxies), `camera` (insta360, gopro, dji), `stitch` (lens models, calibration, blending), `motion` (IMU, stabilization, horizon), `render` (wgpu graph, shaders, color, effects), `audio` (mix, ambisonics, playback), `export` (queue, encode, metadata), `app` (egui), plus an `xtask` for synthetic test media and hardware test runs.

## Full feature set

**Media**: bin with thumbnails, metadata, camera/model detection, and search; a probe panel (container, streams, codecs, color, spherical metadata, telemetry, calibration). Proxies: use the cameras' .lrv files instantly when present, generate short-GOP hardware-encoded proxies in the background otherwise, toggle per viewer, always export from originals.

**Stitching**: fisheye lens models (equidistant/equisolid plus polynomial distortion) with calibration from file metadata where available; a manual fine-tune panel (per-lens rotation, center, radius, FOV) with saved per-camera profiles; feathered seam blending across the overlap; per-lens exposure and white-balance matching; stitch-distance control; nadir/zenith cleanup and an optional nadir patch or logo. Later: optical-flow seam refinement.

**Stabilization**: gyro/accel ingest (reuse telemetry-parser or gyroflow-core where it fits), IMU-to-frame time sync with automatic offset estimation when metadata timing is off, and integration to camera orientation. Modes: full lock, follow heading (smoothed yaw, level horizon), and free, with per-clip smoothing. Horizon leveling has to hold through pitch past ±90° and full flips. The accelerometer gives no usable gravity reference during freefall or high-g moments, so fuse it adaptively and lean on gyro integration whenever its magnitude is far from 1 g. Rotate ambisonic audio by the same orientation. Later: rolling-shutter correction.

**Reframing**: a viewer where dragging grabs the sphere (the point under the cursor stays under it), plus FOV zoom; rectilinear, fisheye, and stereographic (tiny planet) projections with smooth morphs between them; keyframes for orientation, FOV, projection, and morph, set directly from the viewer and refined in a curve editor with easing presets; a full-sphere overview showing where the virtual camera points; look-at target keyframes; synthetic motion blur for fast pans (sub-frame sampling across a virtual shutter). Later: subject tracking that drives the view.

**Timeline**: multi-track video, audio, and title tracks; insert/overwrite, ripple, roll, slip, slide, razor, snapping, markers, J/L cuts, nested sequences; a fast single-clip reframer mode built on the same data model; constant and ramped speed via time-remap curves with frame blending (optical-flow interpolation later), with audio that either stretches with pitch preserved or mutes, per clip; transitions including crossfade, dip to color, and 360-specific ones (spin, whip pan, tiny-planet zoom); multicam sync by audio waveform; JKL shuttle and standard, remappable NLE shortcuts.

**Audio**: mixer with per-clip and per-track gain, fades, keyframed volume, and pan; music beds; loudness normalization to platform targets (EBU R128); first-order ambisonics (ambiX) end to end, decoded to stereo/binaural steered by the virtual camera for flat exports, and rotated and passed through with SA3D for 360 exports; wind-noise reduction and high-pass filtering.

**Color and effects**: correct per-source matrix, primaries, transfer, and range; 10-bit throughout; a linear-light float working space; SDR Rec.709 output. .cube 3D LUTs for log footage; exposure, white balance, contrast, saturation, lift/gamma/gain, and curves; GPU waveform, vectorscope, and histogram; titles and overlays, screen-locked for flat output and world-locked on the sphere for 360; keyframable view-space effects (blur, sharpen, vignette).

**Out of scope unless I say otherwise**: Windows/macOS, NVIDIA-specific paths, stereoscopic 360/VR180, HDR delivery, collaboration/cloud features, a plugin API.

## Milestones

Refine these in PLAN.md with concrete acceptance criteria. Keep the order unless you tell me why. If something is blocked on my samples or hardware, move to the next unblocked milestone and come back.

- **M0 Foundation**: environment check (Vulkan/lavapipe, FFmpeg headers, network, access to my machine); workspace; scripts for fmt, clippy (`-D warnings`), tests, and cargo-deny/audit; the synthetic test-media generator; `spherecut probe`; `cargo xtask hw-test`, with a first run on my machines to confirm the toolchain, Vulkan, and hardware decode work there; docs skeleton; your opening questions and sample request.
- **M1 Viewer**: stitched-equirect decode with hardware acceleration, YUV sampling, the projection shader, drag/zoom, playback with audio, scrubbing; headless `frame` rendering with golden-image tests.
- **M2 Clip reframer**: trim, keyframes, curve editor, flat and 360 export with metadata, export/preview parity tests. This is the first point where I can use it for real, so make it solid.
- **M3 Raw ingest and stitching**: my main camera first, then the others.
- **M4 Stabilization and horizon**, including ambisonic rotation.
- **M5 Timeline**: multi-track editing, projects, undo, autosave, media bin, proxies, speed ramps, transitions.
- **M6 Audio**: mixer, ambisonic decode, loudness, wind reduction.
- **M7 Color and effects**: color pipeline, LUTs, scopes, titles, effects.
- **M8 Delivery**: presets, render queue, export validation, performance work, an install script and .desktop file (then an RPM spec and a PKGBUILD).
- **M9 Advanced**: optical-flow seams and interpolation, subject tracking, rolling-shutter correction, multicam, and zero-copy decode-to-render (dmabuf/Vulkan interop) if profiling says it's worth it.

## Done means

For every milestone:

- It works end to end and you've run it: CLI renders, the Tier 1 suite, and the GUI wherever you can (a headless compositor or Xvfb with lavapipe plus screenshots, or egui_kittest snapshot tests).
- If it touches GPU, decode/encode, GUI, or performance code, a hardware report from at least one AMD machine (plus the Intel laptop once it arrives) has come back clean. Until then, mark it "done pending hardware" in PROGRESS.md and keep going.
- New behavior has tests; clippy and fmt are clean; cargo-deny passes.
- The README's "how to run" and PROGRESS.md are current; the work is committed and pushed.
- Before you call it done, a fresh subagent that didn't write the code reviews it against the acceptance criteria.
- You send me a short note: what changed, exact commands to try, what to look for, and known limitations.

## Verification

- **Math**: unit and property tests (proptest) for every projection round trip (direction ↔ equirect ↔ EAC ↔ fisheye ↔ rectilinear/stereographic), quaternion interpolation continuity, and rational time conversion.
- **GPU**: golden-image tests rendered headless (lavapipe works without a GPU), compared with a tolerance (PSNR/SSIM); keep reference images small.
- **Stitching**: synthetic round trip. Render a known equirect scene through a known dual-fisheye or EAC model, stitch it back, and compare to the original.
- **Stabilization**: synthetic round trip. Render a static scene under a known rotation sequence (including flips) with matching synthetic gyro, stabilize, and assert it comes out static within a threshold.
- **Export**: decode the exported file and compare frames against the preview render; parse the metadata boxes.
- **Robustness**: truncated, corrupt, or unsupported files never panic and always produce a clear error.
- **Performance**: criterion benchmarks on hot paths and an in-app overlay (decode ms, render ms, dropped frames, cache size). Report measured numbers from my machines.

**Performance targets** (measure and report; tell me if one is unrealistic):

- The UI stays at display refresh even when decode falls behind.
- Real-time playback of 5.7K30 raw or stitched footage with hardware decode on odin and huginn; 8K30 in real time at least on proxies.
- A random seek shows a frame in under ~150 ms on proxies.
- Reframed 4K30 exports at or above real time with hardware encode on odin and huginn.
- Frame caches are bounded, sized from available memory, and configurable.

## Hardware test runs

Split the tests into two tiers:

- **Tier 1, runs anywhere**: headless, lavapipe, software decode, synthetic media. All of it must pass in your environment before you hand me anything.
- **Tier 2, needs real hardware**: real Vulkan devices (RADV and ANV), VA-API/Vulkan Video hardware decode and encode, my sample files, the GUI under a real Wayland compositor, and the benchmarks. Gate these so a plain `cargo test` skips them with a stated reason (an `#[ignore]` reason, a cargo feature, or an env var). They must never silently pass where the hardware isn't present.

Build one command, `cargo xtask hw-test`, that I can run without sudo from a fresh clone on any of my machines. It should:

- check its own prerequisites (vulkan-tools, libva-utils, codec support) and print the exact dnf or pacman commands for anything missing;
- record the environment: hostname, kernel, Mesa and FFmpeg versions, GPU, `vulkaninfo --summary`, `vainfo`, and FFmpeg's available hwaccels and encoders;
- build in release mode, run Tier 2 and the benchmarks against `samples/`, and do a short automated GUI smoke run (launch, open a sample, play it, record frame times, quit);
- write one self-contained report to `hw-reports/<host>-<commit>.md` (pass/fail per test, failure output, benchmark numbers, environment) that I can push or attach to the chat.

Ask for a hardware run at the end of every milestone and whenever a change touches decode, encode, GPU, or GUI code. Say which machines, and list anything I should check by eye (smoothness, color, seams, horizon). Batch your requests and keep working on Tier 1 work while you wait. Track benchmark numbers per machine in `docs/perf.md` so regressions show up.

## How to work

- Default to doing, not proposing. Keep moving milestone to milestone without waiting on me. Stop and ask (concisely, with your recommendation) only for decisions that are expensive to reverse (UI toolkit, project file format, a license-affecting dependency), things you can't verify without my hardware or samples, or a real conflict between requirements.
- No fake implementations. Don't stub anything so it looks done; unimplemented features show as unavailable in the UI and are listed in PROGRESS.md. Nothing is done until you've run it.
- Your knowledge of crate APIs (wgpu and egui change often) and of camera formats may be stale. Check current docs and changelogs, and pin versions deliberately.
- This spans many sessions and context compactions. On your first run, save this prompt as `docs/PROMPT.md`. Keep PROGRESS.md (done, in progress, next, known issues, how to run) good enough that a fresh session can pick up cold, and read it plus `git log` at the start of every session.
- Use subagents for parallel research (one per camera format, for example) as well as for the milestone reviews.
- Treat my hardware reports as ground truth. A failing report gets fixed before new features, and when the bug isn't hardware-specific, add a Tier 1 test that reproduces it.
- Code quality: no unwrap/expect on fallible I/O or user data; thiserror in libraries, anyhow at the edges; tracing for logs; unsafe only in FFI and GPU-interop modules, each block with a SAFETY comment; FFmpeg wrapped in a safe RAII layer.

## Start

Read all of this, check your environment, and ask me your opening questions in one batch (main camera, samples, anything that blocks M0–M2). Then write PLAN.md, ARCHITECTURE.md, DECISIONS.md, and PROGRESS.md and begin M0.
