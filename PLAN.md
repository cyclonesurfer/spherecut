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

Agent integration, which Pat added on 2026-10-24, is a track of its own
(A1 to A3, at the end of this file). It doesn't change the milestone
order, and A1 to A3 are never a condition for finishing a milestone. It
does add three criteria it builds on: FFmpeg's confinement (which also
fixes a bug found on the way) and the CLI's JSON contract in M1, and
edits stored as data in M2. From A1 on, each milestone also brings its
new features to agents, which adds a little work to each.

---

## M0 Foundation

- [x] Environment recorded in PROGRESS.md (toolchain, Vulkan/lavapipe, FFmpeg
      version and headers, network reach, device access).
- [x] Cargo workspace with the crates that have real content so far; every
      crate GPL-3.0-or-later, `#![deny(unsafe_op_in_unsafe_fn)]`, no
      `unwrap`/`expect` outside tests.
- [x] `cargo xtask check` runs rustfmt, clippy `-D warnings`, Tier 1 tests,
      cargo-deny and cargo-audit, and fails on any of them.
- [x] `cargo xtask gen-media` writes deterministic synthetic test media to
      `test-media/`: equirect test patterns (lat/long grid with labelled
      directions, a high-frequency pattern for aliasing tests, a seam
      pattern), in H.264 and HEVC, 8-bit and 10-bit, at several frame rates
      including 29.97 and 59.94, with stereo and 4-channel ambiX audio, with
      and without Spherical Video V2 metadata, plus truncated and corrupt
      variants. Re-running produces byte-identical pattern frames.
- [x] `spherecut probe <file>` prints container, streams, codecs, pixel
      format, color (matrix/primaries/transfer/range), frame rate and
      timebase as exact rationals, duration, spherical metadata (st3d/sv3d
      from the file's own boxes), ambisonic metadata (SA3D), and detected
      camera/format. `--json` gives the same as machine-readable JSON.
      Truncated, corrupt and non-media files give a clear error and a
      non-zero exit, never a panic (tested on the generated bad files).
- [x] `spherecut doctor` reports which decoders, encoders and hardware
      paths are available and, for anything missing, the exact
      `dnf`/`pacman` command that fixes it. It shows what works by trying
      it: one frame each of H.264 High, HEVC Main, HEVC Main 10 and AV1 Main
      decoded in software, on VA-API and on Vulkan, and encoded on VA-API.
      It lists the Vulkan devices and flags CPU ones. (Changed 2026-10-23
      from listing VA-API profiles: a listed profile can still fail, as on
      Fedora's stock Mesa. The full profile list is the `vainfo` output in
      the hw-test report. DECISIONS D18.) The GUI will run the same check
      at startup.
- [x] `cargo xtask hw-test` runs without sudo from a fresh clone: checks
      prerequisites (printing the install command for anything missing),
      records the environment (host, kernel, Mesa, FFmpeg, GPU,
      `vulkaninfo --summary`, `vainfo`, FFmpeg hwaccels/encoders), builds in
      release mode, runs Tier 2 tests and benchmarks, and writes one
      self-contained report to `hw-reports/<host>-<commit>.md`. Tier 2 tests
      skip with a stated reason under plain `cargo test` and never pass
      silently without hardware.
- [x] First hardware report back from odin or huginn showing the toolchain,
      a Vulkan device and VA-API HEVC decode working (or exactly what's
      missing).
- [x] Docs: README (what it is, how to build and run), PLAN, ARCHITECTURE,
      DECISIONS, PROGRESS, `docs/PROMPT.md`, `docs/SAMPLES.md`,
      `docs/formats/<camera>.md` research notes, `docs/perf.md` skeleton.

## M1 Viewer

- [x] `MediaBackend` trait with the FFmpeg implementation: open, stream
      info, seek (frame-accurate, by PTS), decode to the frame's native
      planar or semi-planar YUV (NV12/P010 from VA-API), uploaded
      unconverted, both software and VA-API (with automatic software
      fallback), audio decode. (Reworded from "decode to NV12/P010
      frames" in M1, DECISIONS D30 and D31: no CPU colour conversion, as
      meant.)
- [x] FFmpeg opens only the file Spherecut opened: `protocol_whitelist`
      is `file`, demuxers are limited to the containers Spherecut supports
      (MP4/MOV and Matroska for now, widened only by a DECISIONS entry),
      nested opens are refused through `io_open`, and every decoder gets a
      `max_pixels` limit. Clippy forbids `std::net`'s TCP and UDP types.
      Tier 1 tests: an HLS playlist whose segment lies outside its folder,
      an ffconcat file, a playlist with an `http://` segment and the path
      `http://127.0.0.1:1/x.mp4` are each refused with the reason, and
      probe and `frame` run under `strace -f` open no `AF_INET` or
      `AF_INET6` socket. strace becomes a prerequisite that `cargo xtask
      check` checks, so that test fails rather than skips without it.
      probe then refuses other containers by name instead of reporting
      them with `structure: none`; README, D15 and the `lrc` test change
      with it. (Found on 2026-10-24: M0's probe follows an HLS playlist to
      a file outside its folder.)
- [x] Projection shader (WGSL) sampling the frame's planes directly (deep
      planes normalised on the GPU, RGB only in the shader), with
      per-source YUV→RGB matrix and range, into a linear-light float target,
      output as sRGB/Rec.709. Equirect source; rectilinear, fisheye and
      stereographic output with morph.
- [x] No seam line at ±180° and no shimmer in wide-FOV/tiny-planet views:
      analytic LOD (explicit gradients) plus mipmaps or supersampling,
      proven by golden images of the high-frequency pattern.
- [x] `spherecut frame <file> --time <t> --yaw/--pitch/--roll --fov
      --projection --size WxH -o out.png` renders one frame headless.
- [x] The CLI's JSON contract, for agents and scripts (probe, doctor,
      frame): with `--json`, stdout carries exactly one compact JSON
      document per run, on success and on failure, with a schema name and
      version, an `ok` flag, and errors with a stable `code`. The shape is
      the same for one file or many, and times are written as
      `{exact, seconds}`, plus `frame` when the time is a frame of a video
      stream (the shape MCP uses from A1). stderr carries only logs. Exit
      codes: 0 ok; 1 an input or check failed, doctor's errors included
      (as in D18 and the README); 2 usage error; 3 a codec or device the
      command itself needs is missing; 4 cancelled; each tested. A command
      that writes refuses an existing output unless given `--overwrite`.
- [x] Golden-image tests on lavapipe: each projection, the seam, the poles,
      10-bit input, limited vs full range, compared by PSNR/SSIM with stored
      small references. GPU output matches the CPU reference projection in
      `spherecut-sphere` within tolerance.
- [x] GUI (eframe + wgpu + dock): open a file, viewer panel with a custom
      wgpu paint callback, drag-to-grab-the-sphere (the point under the
      cursor stays under it, tested numerically), scroll/pinch FOV zoom,
      projection switch, play/pause, scrubbing, frame step, audio playback
      with the audio clock driving A/V sync. (Pinch is two-finger scroll
      and Ctrl+scroll until winit has touchpad gestures on Wayland, D36.)
- [x] Decode, audio and texture upload on workers; UI never blocks on them
      (asserted by a test harness that injects a slow decoder). Bounded frame
      cache sized from available memory, configurable.
- [x] Performance overlay: decode ms, upload ms, render ms, dropped frames,
      cache size.
- [x] Startup codec/hardware check in the GUI with the doctor's fix
      instructions.
- [ ] HiDPI and fractional scaling correct; works under GNOME and Hyprland
      (hardware report + screenshots). Pending hardware: GNOME at 1.0 on
      odin passes (H3), and at 125 % on odin (2026-10-30); Hyprland
      waits for a machine.
- [ ] Hardware: 5.7K30 stitched equirect plays in real time on odin and
      huginn with VA-API decode; numbers in `docs/perf.md`. Vulkan Video
      HEVC is measured too (doctor's decode check, a bit-exact check on
      the X5 samples and the sample benchmark), because odin's discrete
      RDNA3 may decode HEVC with FFmpeg's Vulkan path where huginn's RADV
      can't. Pending hardware: on odin the noisy 5.7K clips play in real
      time with Vulkan Video, which `Auto` picks there, headless and in
      the window (H3, D42), but not with VA-API, whose copy decodes them
      at 14 to 22 fps; huginn's run and clip 12, a stitched camera
      export, are still to come.
- [ ] Zero-copy proof (Pat, 2026-10-24, from the stack review): a Tier 2
      test imports VA-API NV12 and P010 frames from an X5 sample into wgpu
      as DMA-BUF planes on huginn and odin, renders one frame, and matches
      the copy path's output; a benchmark plays X5 8K30 both ways. It runs
      with M1's hardware report. Whether zero-copy becomes the default is
      decided before M3, when raw 8K dual fisheye has to play. The copy
      path stays as the fallback, and the overlay and doctor say which
      path is in use (D25). Decided for Vulkan Video on 2026-10-30 (Pat,
      from odin's window): it decodes on the renderer's own device by
      default where that can be shared; VA-API's import stays opt-in
      until huginn's numbers. Pending hardware: odin's test passes (one
      unexplained mismatch in about a dozen runs, PROGRESS's known
      issues), and huginn's run is still to come.

## M2 Clip reframer

The first version Pat can use for real.

- [x] Single-clip reframer mode on the core data model (a sequence with one
      clip), so M5's timeline extends it rather than replacing it.
- [x] Trim in/out with exact rational times; markers.
- [x] Keyframes for orientation (quaternion), FOV, projection and morph;
      set from the viewer; interpolation on SO(3) (squad or a C¹ spline)
      with easing presets; property tests for continuity and no flips
      through ±180°.
- [x] Curve editor panel (FOV, morph, and yaw/pitch/roll as displayed
      views of the quaternion track) with easing presets.
- [x] Look-at target keyframes.
- [x] Full-sphere overview showing where the virtual camera points.
- [x] Every edit is a value of one `Edit` enum in `spherecut-core` (serde,
      versioned with the project schema), applied only through
      `Project::apply(&[Edit])`: all or none, no I/O. The GUI changes the
      project only by building `Edit`s, and a drag becomes one undo step
      when it ends. Clips, keyframes, markers and look-at targets have IDs
      that are never reused, and edits name IDs, not positions. The
      project has a revision number, and each undo step has a label and an
      origin (Pat or an agent). Property tests: undoing a random sequence
      of edits gives back the original, save then load gives an equal
      project, and a rejected edit changes nothing. This is what agent
      integration A2 builds on.
- [ ] Project save/load in a format chosen with Pat and recorded in
      DECISIONS.md (JSON is the leaning, since agents and `jq` read it),
      with a schema version and relative media paths; undo/redo for every
      edit.
- [x] Flat export: 16:9, 9:16, 1:1, 4:5 at 1080p/1440p/4K, project frame
      rate, H.264/HEVC/AV1 via VA-API with libx264/libx265/SVT-AV1
      fallback; audio passed through or re-encoded.
- [x] 360 export: equirect up to 7680×3840 with st3d/sv3d (and SA3D for
      ambiX), written by our own box writer and validated by re-parsing.
- [x] Export runs in the background with progress and cancel; the UI stays
      responsive.
- [x] Parity tests: decode exported frames and compare against
      `spherecut frame` renders of the same times (PSNR threshold), flat and
      360.
- [x] `spherecut render <project> -o out.mp4 [--preset] [--overwrite]
      [--json]` headless. With `--json` it prints one progress line a
      second and a final result line. An existing output is refused without
      `--overwrite`, and Ctrl+C leaves no partial file.
- [x] Synthetic motion blur for fast pans (sub-frame sampling), off by
      default, with a golden test.
- [ ] Hardware: reframed 4K30 exports at or above real time with VA-API on
      odin and huginn.
- [ ] Toolkit check at the end of M2 (Pat, 2026-10-24, from the stack
      review): a timeline prototype with about 500 clips on 6 tracks, drag
      with snapping and ripple trim, holds display refresh on huginn and
      odin with fractional scaling, with UI time per frame in the overlay.
      If it doesn't, the problem is written up and Pat decides before any
      switch, with Qt/QML through cxx-qt as the prepared alternative. The
      render core and the project model hold no UI types, so a switch
      would replace only `spherecut-app` (D26).

## M3 Raw ingest and stitching

Main camera first (the Insta360 X5, Pat confirmed on 2026-11-01), then the others.

How it is built: Pat asked on 2026-10-30 for ultracode from M3 on, so
each M3 step (understanding, design, implementation, review) runs as a
multi-agent workflow. A session that starts M3 turns ultracode on in its
settings, or has "ultracode" in its prompt, before the first workflow.

- [ ] Format notes in `docs/formats/<camera>.md` verified against Pat's
      samples, per model and firmware.
- [ ] Container and metadata parsing (vendor trailer / GPMF / DJI metadata)
      via telemetry-parser where it fits; frame-locked decode of lens pairs.
      (Clarified 2026-11-01, not amended: in M3 it does not fit. M3 reads
      the X5's trailer and DJI's calibration block with its own parsers
      and does not use telemetry-parser, not even as a test oracle: git
      sources are forbidden, crates.io's 0.2.x predates the X5, and its
      geometry is Studio-derived. Whether M4 uses it to parse the IMU
      stays open. Section 20 of `docs/design/m3-plan.md` and the plan's
      D62 (its section 23), which DECISIONS.md records at M3.21;
      DECISIONS, Open.)
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
view, rolling-shutter correction, multicam sync by audio, Vulkan Video
decode where drivers support it, and polish of the zero-copy path. (The
zero-copy proof moved to M1 on 2026-10-24, with the decision before M3;
D25.)

---

## Agent integration (track A, beside the milestones)

*Added 2026-10-24 at Pat's request: "I want all the agents to be able to
use it." DECISIONS D23 has the design reasons, and
`docs/research/agents-2026-10.md` the research behind them.*

Any AI agent on Pat's machine that can start an MCP server or run a
command will be able to look at footage, check files, edit projects and
render, through the same core and the same undo as the GUI. All eight
agents checked on 2026-10-24 can. There are two ways in:

- **`spherecut mcp`**, a Model Context Protocol server on stdin/stdout.
  Claude Code, Codex, Gemini CLI, Copilot CLI, Cursor, Zed, goose and
  opencode can all start a local stdio MCP server. The MCP revisions
  served were checked against Claude Code, Codex and Gemini CLI;
  `docs/agents.md` records what each other client negotiated when it was
  tried. Not every client shows images (Zed's docs don't mention them,
  and goose and opencode weren't confirmed), so every image result also
  says in text what it shows.
- **The CLI with `--json`**, for agents and scripts that only run
  commands. `spherecut call <tool> '<json>'` runs any MCP tool from the
  shell with the same arguments and the same structured output, and
  writes images to `--out <dir>` (the current directory by default)
  without overwriting.

Agents in a browser chat or a cloud workspace can't reach the copy on
Pat's machine, because Spherecut opens no port. A cloud agent can still
build and run the CLI on files in its own workspace, without Pat's GPU.

Spherecut itself still makes no network requests. Whatever a tool
returns does go to the agent's model provider, though, frames of Pat's
footage included, and `docs/agents.md` says so first.

This is an automation interface, not the plugin API the prompt puts out
of scope. Nothing from outside is loaded or run inside Spherecut. Tools
take only data (paths, times, views, edits), never code, expressions,
FFmpeg filter strings or shell commands, and no tool starts a process or
deletes or moves a file.

Staging: three small pieces land inside milestones because the rest
builds on them: FFmpeg's confinement and the CLI's JSON contract in M1,
and edits stored as data in M2. A1 is mostly Tier 1 work, done once M1's
code is in, while M1's hardware report is pending. That time would
otherwise go to M2, so M2 lands later by roughly the time A1 takes; Pat
chose this order on 2026-10-24. A2 follows M2 and
A3 follows M5. None of A1 to A3 is a condition for finishing a
milestone.

### A1 Look and check (after M1)

What Pat gets: an agent can go through a folder of footage, look at any
moment from any direction, find where something happens, explain
stutter or audio problems, and check a file's 360 metadata. It can't
change or render anything yet.

- [x] `spherecut mcp` serves MCP revisions 2025-06-18 and 2025-11-25
      through `initialize`, which every client checked can use (Claude
      Code asks for 2026-07-28 first and falls back). It also serves
      2026-07-28 through `server/discover` if the chosen MCP layer
      provides it; with our own layer, that waits until a client needs
      it. It answers within 1 s, before any FFmpeg or GPU setup. The
      supported revisions are rechecked at every milestone review.
- [x] A small, fixed set of tools: at most 12 through M9, and a test fails
      past that or past 12 KB of `tools/list` JSON. A1 and A2 list 12
      tools between them, so A3 works through `project` and `look`. A1
      has these:
      - `probe` takes files or a folder. By default it gives a summary of
        about 300 bytes per clip (duration, size, frame rate, codec, bit
        depth, projection and metadata status, camera, audio layout,
        problems), and the full report on request. A folder groups lens
        pairs, LRV proxies and chapters into clips, sorted by recording
        time, a page at a time.
      - `look` renders one image of one or more times and views: a single
        view, the full sphere with a labelled yaw/pitch grid, the six cube
        directions, or a contact sheet of up to 24 times. Each tile is
        labelled with its time and direction, and the text result lists
        each tile's exact time and view and the decode path used
        (`vaapi`, `vulkan` or `software`), so it still makes sense to a
        client that drops images. It stops with an error after 50 s, under
        Codex's 60 s default call limit.
      - `analyze` gives text summaries over a time range in buckets (at
        most 200): `timing` (frame durations, variable frame rate,
        duplicate or missing frames, keyframe interval, bitrate), `motion`
        (frame difference, likely cuts) and `audio` (peak, RMS, clipping,
        silence). M4 adds `telemetry` (rotation rate, freefall, high g),
        and M6 adds loudness (EBU R128) and the loudest direction in ambiX
        audio, using M6's own code.
      - `check` validates a file's spherical and ambisonic metadata against
        its streams and, optionally, a target (YouTube 360, 16:9, vertical,
        archive). It gives pass or fail per rule, with the fix for each
        failure, in under 2 KB. M2 and M8 add their export checks to it.
      - `doctor` is the CLI's doctor, plus a `file` argument that names the
        decode path that file gets (and why hardware refused it) and, on
        request, its decode speed against real time.
- [x] Tool input schemas are JSON Schema draft 2020-12 with an object at
      the root and no root-level `oneOf`, `anyOf` or `allOf` (so `look`'s
      mode is a field, not a tagged enum). Property names are snake_case
      ASCII, each description is under 4,096 characters, and tools are
      listed in a fixed order. Claude Code drops or mangles tools that
      break these rules, so a test checks every advertised schema.
- [x] Images sent inline default to 768 px on the long edge as JPEG, are
      capped at 1568 px and 24 tiles, and state their pixel size and bytes.
      PNG is available on request. A larger image is written to a file
      under the write root and returned as a path; without
      `--allow-write`, a request over the cap is refused, naming the limit
      and the flag. FFmpeg encodes the images, so no image crate is added.
- [x] Conventions, stated in the tool descriptions. Times are exact: a
      string (`"12.5"`, `"1:02.5"`, `"370f"`, `"30000/1001"`) or a
      `[num, den]` pair. A bare JSON number is refused, with the string to
      send instead. Every time comes back as `{exact, seconds}`, plus
      `frame` when it is a frame of a video stream, the same shape as the
      CLI's `--json`. Angles are in degrees, the descriptions say which
      way positive yaw and pitch turn and where yaw 0 is in the equirect,
      and `fov` is horizontal unless `vfov` is given.
- [x] Errors an agent can act on: a failed call is a tool result marked
      `isError`, with a stable `code`, the argument at fault, the valid
      range or choices and the next step, in under 500 characters.
      JSON-RPC errors are only for malformed or unknown requests.
- [x] Safety:
      - Read-only by default. `--allow-write --write-root <dir>` adds the
        tools that write. Footage is read only from `--read-root <dir>`
        (repeatable; the working directory by default) and from the write
        root, so `look`, `check` and `compare` work on renders.
      - The kernel enforces the roots: every input and output is opened
        with `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` below its
        root, and probe, the box reader and FFmpeg all read that one open
        file. Each of these is refused, with a test: `..`, an absolute
        path outside, a symlink out of the root, a dangling symlink,
        `/proc/self/fd/N`, and a file swapped for a symlink between calls.
      - The roots limit what Spherecut opens for an agent, not what the
        agent itself can do: an agent that can run shell commands can
        read Pat's files without Spherecut. The roots keep Spherecut from
        becoming a way around the agent's own sandbox. `docs/agents.md`
        says this next to the network note.
      - Nothing is overwritten unless the call asks. Even then, a camera
        original (`.insv`, `.360`, `.lrv`, DJI raw), a source of an open
        project, and anything under a read root but outside the write
        root are never replaced, and an output path ending in a symlink is
        refused.
      - Text read from media files (tags, vendor strings, file names,
        FFmpeg log lines) appears only in JSON string fields, capped at
        1 KiB, with control, ANSI and bidi characters replaced. It never
        appears in tool descriptions, server instructions or error
        messages, and the tool descriptions say that these fields and the
        pixels of rendered frames are untrusted content. From M3/M4, GPS
        tracks, camera serial numbers and owner names are left out unless
        the call asks for them.
      - Every tool carries MCP annotations: `readOnlyHint`,
        `destructiveHint` where it can replace a file, and `openWorldHint`
        false.
      - Limits live in one module, are listed in `docs/agents.md` and each
        has a test: message size, image size, decoder `max_pixels`, files
        per call, edits per call, and `look`'s 50 s.
- [x] Each operation has one request type and one response type, shared by
      MCP and `spherecut call`. A test runs every tool both ways on the
      same input and requires identical structured JSON, apart from
      images, which `call` writes to files.
- [x] Teaching agents the workflow: the server's `instructions` (under
      1,500 characters) give the conventions and the cheap-first order:
      probe, then a contact sheet or the sphere, then `analyze`, then
      single views. An Agent Skills `SKILL.md`, which Claude Code, Codex,
      Gemini CLI and Copilot CLI all read, holds three recipes: find where
      a moving subject is over time, validate a 360 export, and find why a
      file stutters. (A2 extends the first into reframing the subject to
      9:16 and rendering it.) `spherecut agents install-skill` links it
      into `~/.claude/skills` and `~/.agents/skills` when Pat runs it.
      `docs/agents.md` gives the exact setup for each client, with the
      timeouts to raise and the environment variables to forward (Codex
      passes servers only a short list, so the VA-API variables need
      forwarding), and, for a client installed as a Flatpak, how to start
      the host's `spherecut` through `flatpak-spawn --host`. Nothing tells
      Pat to run `npx`.
- [x] Tier 1 tests, with our own scripted client written in Rust (the
      official conformance suite and MCP Inspector are npm tools): a
      session at 2025-06-18 and one at 2025-11-25 (`initialize`,
      `notifications/initialized`, `ping`, `tools/list`, `tools/call`); if
      2026-07-28 is served, a session with `server/discover`, then
      `tools/list` and `tools/call` with `_meta`, checking `resultType`,
      `ttlMs`, `cacheScope` and the tool order; an unknown revision (a
      legacy `initialize` gets 2025-11-25 back, a modern request gets
      -32022 with the supported list); an unknown tool (-32602); bad
      arguments (an `isError` result); malformed JSON (its documented
      outcome, and the next request still answered); cancellation (no
      response for that id, and the work stops); stdin closing (the server
      exits within 2 s); every stdout line parsing as one JSON-RPC
      message; a run with the environment cut down to Codex's list; and
      the JSON-RPC reader fuzzed with proptest. The three recipes replay on
      synthetic media and find the planted answers within a stated budget,
      counted as text characters / 4 plus ceil(w/28) × ceil(h/28) per
      image. gen-media adds a clip with frame-rate jitter and duplicated
      frames; the grid clips' moving marker (D9) serves the first recipe.
- [ ] Hardware: a Tier 2 MCP session decodes one of Pat's samples and
      checks the result says `vaapi`; without hardware it skips with the
      reason and never passes. Pat runs the recipes on the samples with two
      agents, one using each kind of session where possible (Claude Code,
      and Codex or Gemini CLI). PROGRESS.md records the client versions,
      the revision each negotiated, the number of calls and the output
      size, and whether a `look` at the default size and at the 1568 px
      cap reached the model in each (not refused by Claude Code's output
      limit, not replaced by a placeholder).

### A2 Edit and render (after M2)

What Pat gets: an agent can open a project, change trims, keyframes and
views, undo its own changes, render, and check the result, while the
project isn't open in the GUI.

- [x] New tools: `project` (create, open, a summary, or one part such as
      `clips[0].view`, never the whole timeline by default), `edit`,
      `undo`, `redo`, `render`, `job` and `compare`. The project tools are
      stateless: every call names the project file, as MCP 2026-07-28
      expects, so a restarted server loses no project state. There is no
      save tool: `project` create, `edit`, `undo` and `redo` load the
      named project and save it in the same call. The project file must
      be under the write root, and that save doesn't count as an
      overwrite for the safety rules.
- [x] `edit` takes a list of M2's `Edit` values and applies them all or
      none, as one undo step labelled with the agent's name. It takes
      `dry_run` and `if_revision`; a stale revision is refused with the
      current one, and the agent reads the project again. One edit can set
      a whole view path (time, orientation and FOV per point), for "follow
      the rider" until M9's tracking exists.
- [x] Undo history is saved beside the project and records the revision
      each step produced; the GUI reads and writes the same history.
      `undo` and `redo` take `if_revision` like `edit`. An agent's `undo`
      undoes only a step an agent made, and only when that step is on top
      and the project is still at its revision; otherwise it is refused
      (`not_top` or `stale_revision`) and nothing changes. Test: an agent
      edit, then a GUI edit and save, then the agent's `undo` is refused
      and the GUI edit survives.
- [x] Opening a project to write takes an advisory lock. A project the GUI
      has open is read-only to `spherecut mcp`: until A3 the error tells
      the agent to ask Pat to close it in the GUI, and from A3 on to
      attach. Each save checks that the file hasn't changed on disk since
      the call loaded it, and writes atomically (D16). Test: two
      processes, one saves between the other's load and save, and the
      second save is refused.
- [x] `render` checks its arguments and returns a job id within 1 s.
      `job` reports frames done and total, frames per second, the time
      left and, when finished, the `check` report. It takes `wait` (at
      most 50 s, under Codex's 60 s default, the shortest limit found;
      `docs/agents.md` says how to raise a lower one) and `cancel`. One
      render runs at a time, with at most 4 queued. The output appears
      only when it is finished and validated. A render is refused when
      free space is below an estimate from the preset's bitrate and the
      duration; if the disk fills anyway, it stops with an error and
      leaves no file. `range` and a draft size allow a quick preview
      first. Render jobs are not stateless: when the server exits or its
      stdin closes, its jobs are cancelled and leave no files, and `job`
      with an id from an earlier server returns `job_lost`. If the server
      is killed, its leftover temporary file is removed when the next
      render starts. Tests on lavapipe: cancel takes under 2 s and leaves
      no file, stdin closing mid-render leaves no file, and progress only
      increases.
- [x] `compare` takes two files, or a file and a project, and returns
      PSNR/SSIM per sampled time and the worst time, with an optional
      difference sheet, using the parity tests' metric code. `look` and
      `check` accept a project as well as a file.
- [x] A test applies one of every `Edit` variant through MCP. GUI actions
      that aren't edits are listed in `docs/agents.md`, each with its tool
      or the reason it is GUI-only. Once M5's shortcut map gives the GUI a
      list of actions, a test fails when a new action is in neither list;
      until then, each milestone review checks it.

A2 was built on 2026-10-31 (`docs/design/a2-plan.md`, D57 to D59;
evidence in PROGRESS.md, "A2 criteria ticked"). Its plan reads these
rows differently in a few places (section 1.1): `redo` is `undo` with
`redo: true`, so A1 and A2 list eleven tools; `job`'s `wait` is at most
45 s, inside a call's 50 s; `edit`, `undo`, `render` and `job` are
listed only with a write root; and `add_clip` and `remove_clip` are
refused under M2's one clip, so the every-variant test checks their
refusal. A Tier 2 MCP session renders on VA-API and compares the file
with its project (`mcp_render_vaapi`); its run on odin is to come.

### A3 Work alongside Pat in the GUI (after M5)

What Pat gets: with attach turned on, an agent can see what Pat has open
in the GUI and make edits there that Pat sees as they happen and can undo
with Ctrl+Z.

- [ ] Attach is off until Pat turns it on in the GUI. Then the GUI listens
      on `$XDG_RUNTIME_DIR/spherecut/<pid>.sock` (directory 0700, socket
      0600), or under `/run/user/<uid>` when the variable isn't passed and
      that directory is Pat's and 0700. Never `/tmp`, the abstract
      namespace or TCP. The GUI checks the peer's uid with `SO_PEERCRED`
      and asks Pat to accept each new agent, showing the name it reports
      and its executable. Agents start `spherecut mcp --attach`, which
      relays the same MCP messages to the socket. Pat confirms first that
      a local socket like this is IPC, not the network access Pat's rule
      forbids (DECISIONS, Open).
- [ ] The agent can see the open project, the playhead, the selection, the
      in and out marks, the performance overlay's counters, and a `look`
      of the viewer as displayed. That view is rendered again from the
      view state off the UI thread, never captured from the screen, and at
      most once a second while Pat plays. A kittest test keeps UI frame
      times within budget while an agent asks for frames in a loop.
- [ ] Agent edits are applied on the UI thread between frames, never in
      the middle of one of Pat's gestures, from a bounded queue. Pat's
      Ctrl+Z undoes whatever step is on top; an agent's `undo` follows
      A2's rule. If Pat closes or switches the project, the agent's next
      call says so. One agent is attached at a time; the GUI shows which,
      and can disconnect it. kittest Tier 1 tests cover each case.

### Every milestone after A1

New features reach agents as new `Edit` values, `analyze` kinds, `check`
targets and fields, not as new tools, with schemas, docs and tests in the
same milestone, so each milestone carries a little more work. The tool
schemas are a public interface: until 1.0 they may change between
milestones, and `docs/agents.md` lists the changes.
