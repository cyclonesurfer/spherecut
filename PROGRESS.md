# Progress

Updated 2026-10-28, with FFmpeg confined to the file Spherecut opened (D24), the CLI's JSON contract for probe and doctor (D27), odin's first hardware report, M1 slice 2a: `MediaBackend` with frame-exact seek in software, native pixel layouts and the colour defaults (D29, D30), slice 2b: hardware decoding through the backend, with fallback to software, the pooled download, the render node lookup and the Tier 2 seek tests (D30), slice 3a: the render core, checked on lavapipe against its CPU reference (D31), and slice 3b: the golden images with their negative controls and the render Tier 2 tests (D32), with the unsafe rule at every crate root (D28), and FFmpeg 8.1.3: the build break Pat found on odin, two faults Tier 1 found against 8.1.3 and one its review found, and `cargo xtask compat`, which lints and links against other FFmpeg builds (D2), and slice 4: `spherecut frame`, with the rest of the JSON contract (exit statuses 3 and 4, SIGINT, `--overwrite`), `TimeSpec` and `core::fs::TempFile` (D16, D27, D39). Milestone criteria are in `PLAN.md`;
decisions and their reasons in `DECISIONS.md`.

## Status

| Milestone | State |
|---|---|
| M0 Foundation | **Done** (2026-10-23). The huginn report at 4cd8a0f is clean: every Tier 2 test passes but one, which skips because RADV on Strix Halo has no Vulkan Video H.264 or HEVC decode. Evidence below |
| M1 Viewer | In progress. `VideoDecoder` (software, VA-API, Vulkan) is its first slice. FFmpeg's confinement (D24) is the second; its criterion is ticked since slice 4 gave `spherecut frame` its strace test. The CLI's JSON contract for `probe` and `doctor` (D27, `docs/cli-json.md`) is the third; its criterion is ticked since slice 4 brought `frame` into it, with exit statuses 3 and 4, SIGINT and `--overwrite` (evidence under "M1 criteria ticked", below). Slice 2a is the fourth: `core::video` and `core::cancel`, `media::backend` with `FfmpegBackend` and the frame-exact seek of plan section 3.3 in software (D29), native pixel layouts, swscale for the rest and the colour default rules (D30), synth's new clips and its `SyntheticBackend` and `GatedBackend`. The seek oracle (`crates/synth/tests/seek.rs`) is exact on every clip of the set, among them clips whose timestamps tie or go backwards and variable-rate Matroska files with a constant default duration; it checks what each seek cost and every run link, and its negative controls miss. The slice's review found 16 faults (wrong frames returned as `Exact` after a cut, in broken timestamps and in variable-rate Matroska, false run links when catching up, a duration missing for Matroska, an index trusted before Cues are read); all are fixed, each with a test that fails without its fix where one can show it (D29, D30). Its second review found 10 more (an open-GOP H.264 leading picture and a RADL picture returned after a seek, seeks past an MP4 cut, an MP4 spliced by its edit list read as constant-rate, resuming after a stopped seek in tied and backwards streams, an FFmpeg log capture that lost its lines to another thread, a test that a change in load could fail, and an overflow on a far-off Cues entry); all are fixed too, with tests where one can show it (D29). Slice 2b is the fifth: `DecodeMode::Hardware` decodes through the backend on VA-API or Vulkan Video, on one device per kind per backend opened on the render node `media::hw` finds (bus id, then PCI ids, then `renderD128`; `--render-node` overrides it), single-threaded, with frames kept on the device until handed out and downloaded into a pool of 256-byte-aligned buffers. `Fallback::Software` opens in software when the device can't be opened or turns the stream down, and reopens in software when it fails mid-stream, going on from the frame after the last one returned with no gap and no repeat; `Fallback::Fail` returns the error. A device's refusal or failure is its own error (`HwUnsupported`, the new `HwFailed`), never the file's damage. Media's `fault-injection` feature, turned on only by synth's tests, fails any frame as the device would, and the Tier 1 fallback tests (`crates/synth/tests/fallback.rs`) fail every frame of H.264 and HEVC clips while seeking, catching up and playing; they also fall back on lavapipe and on a render node that isn't there. The Tier 2 tests the plan names are written and listed (`vaapi_seek_matches_software`, `vaapi_colorinfo_matches_software`, `vaapi_download_pool_reuses_buffers`, `bench_seek_latency`, and Vulkan Video versions of the first three), and here they skip with their reasons; their checks pass on software standing in for a device and fail on one that fails. The slice's review found 4 faults (a stream cropped on the left came off the device narrower than software's frame, or failed on its first download at 10 bits, since FFmpeg's aligned cropping keeps columns by the pixel format's own rule; the pool tests couldn't tell a pool from a fresh buffer per frame; a device that won't open was named `vaapi`, and the Vulkan device's log line named the node asked rather than the GPU taken); all are fixed, with tests where one can show it. Both paths now crop exactly as the stream says, and the Tier 2 seek and colour sets have clips cropped on the left (D30). Slice 2 is done once 2b merges; its hardware results come with H1. Slice 3a is the sixth: `spherecut-render` on wgpu 30.0.1 (Vulkan only) with `rust-version` 1.95 (D1): the device (`Gpu`, `AdapterChoice`, `GpuCaps`, `RenderError` with contract codes), canonical plane textures (deep planes through an integer scratch and an ingest pass into f16) with area mips capped near 64 px, GPU slots, the `Uploader` (`write_texture` or a staging belt, one submit per frame), the projection shader (every projection and morphs, the shader's own LOD, colour from `ColorInfo`), the encode pass, `render_still` and the f64 CPU reference (D31), with sphere's `pixel`, `orient`, `uv_gradient` and `project_morph` and testkit's `compare` and `fnv64`. On lavapipe `render_still` matches the reference at 49.6 to 59.0 dB through every projection and ten plane layouts at 8 to 12 bits, both ranges, three matrices and three sitings, and the mips match an independent area mean; negative controls tell level 0 from trilinear and one siting from another (`crates/render/tests/`). The slice's first review found 11 faults; all are fixed, each with a test that fails without its fix where one can show it: lavapipe truncates an f32 stored into an f16 target, so half the 10-bit codes ingested a half low (the shaders now round every f16 store to the nearest half themselves); an upload's GPU time left out the ingest; a stereo frame's other eye bled in across the shown eye's edges (only the shown eye is uploaded now); the chroma siting had no check that didn't share the renderer's own arithmetic, and 4:1:1's factor was wrong at small widths; readback failed past wgpu's 256 MiB buffer limit; a lost device panicked in wgpu's error handler (every entry point now returns `gpu_failed`); and D31, D22, ARCHITECTURE and a test comment said things that weren't so (the GUI's device path, the crate count, FFmpeg in render's tests, the belt branch a test covered). An odd-width 360° frame's chroma step at the seam is recorded as a limit (D31). Slice 3b is the seventh: the golden images (`crates/render/tests/goldens.rs`, D32). One test runs every row of plan section 6 on lavapipe from synth's in-memory planes: six views stored as PNGs that must come back pixel for pixel at the Mesa build they were blessed on, every view against the f64 reference at floors calibrated when blessing, and minified views against synth's new `render_view_ideal`, the supersampled pattern itself. Each row's negative control is asserted on every run: implicit derivatives give 175 to 288 bad pixels at the seam and the poles where the analytic LOD gives none, level 0 shimmers 3.3 to 5.0 times as much as the ideal, and a swapped range, matrix or siting misses by 22 LSB, 19.5 dB and 53 LSB. A manifest records each row's driver build, source hash, view, thresholds and measurements; a changed source is reported as one and never compared as a render; and blessing (`SPHERECUT_UPDATE_GOLDENS=1`) writes nothing unless every row passes and every control fails. The evidence changed four of the plan's rules: the ideal comparisons cover only pixels 2.5 levels down or more, where level 0 must alias (over a whole view isotropic trilinear is softer than the ideal, and level 0 nearer it); range and siting are asserted at 10 bits, with 8 bits recorded, since lavapipe filters 8-bit planes to whole codes; `passthrough_exact` holds limited range to an independent model of the chain, and to round((k − 16) · 255 / 219) except within the f16 store's 0.0547 codes of a tie (86, 159 and 165 come out one code off); and each pole is turned to lie inside a pixel quad, which pitch ±90 exactly doesn't do. The Tier 2 tests `list_adapters_and_features`, `goldens_on_gpu_match_reference`, `goldens_on_host_llvmpipe` and `bench_upload_5k7` are written, each with a Tier 1 run on lavapipe; here the first two and the bench skip with their reasons, and `goldens_on_host_llvmpipe` passes, lavapipe being this machine's llvmpipe. Every crate root now forbids `unsafe`, except media's and the `spherecut` binary's, which deny it and allow it only in their FFI modules (D28). The slice's review found 10 faults; all are fixed, each with a test that fails without its fix where one can show it: a matrix or siting swapped inside render's colour code passed `matrix_601_709` and `siting_left_center` and was blessed, since the CPU reference shares that code (both rows now have truths outside it: the two matrices' codings of one picture must render alike, and smooth 4:2:0 chroma must match its 4:4:4 self); a blessing could lower any calibrated floor (it now prints them old → new and refuses a drop of more than 0.5 dB unless told `SPHERECUT_UPDATE_GOLDENS=lower`); the blessing command exited 101 after a blessing that had written everything (the harness's own tests read `tests/goldens` while it was rewritten, and now bless into scratch directories); three manifest entries lacked their sources or views; the controls' "over" bounds passed at the bound; `bench_upload_5k7` couldn't see a block made and freed within a step, which is its second trigger (it reads the allocator in flight too, and also runs two steps in flight), and timed 5.7K NV12 on the copy a decoded frame doesn't take (its frames now have the pooled download's stride); a GPU named by `SPHERECUT_ADAPTER` that refused a device showed as skipped; `round_f16` had no test of the subnormal halves and stored the largest half where the nearest is infinite; and a test comment was stale (D31, D32, m1-plan amendment 11). No golden image changed. Slice 3 is done. On 2026-10-24 main stopped compiling against FFmpeg 8 (3a8047d's swscale flags; fixed in 67fa1fd), which odin's `hw-test` found and nothing here had checked: Tier 1 now runs against an 8.1.3 built like Pat's, where it found that on one thread, as hardware decoders run, FFmpeg 7.1 and later's HEVC decoder gave a frame from before a seek after it (fixed, with a test, D29); odin's second run (PR #4) failed both GPU seek tests on it, and the download pool tests on a fault limit that counted the driver's faults (m1-plan amendment 12). Its review found that from 8.0 a damaged HEVC reference picture lost the rest of its GOP without a `Damaged` event, and an exact seek into it returned an earlier frame as `Exact`, so slice 2a's "damage the decode goes on past is an event" didn't hold on Pat's FFmpeg (fixed, with a test; on 7.1.x such damage is still silent, D29). Tier 1 against a 7.1.3 found that FFmpeg 7.0 to 8.1.2 leave an audio stream whose decoder they couldn't open without channels, which probe took for "has no channels" rather than the decoder's refusal, so a feature FFmpeg lacks became damage (fixed, with a test, D2, D15). `cargo xtask compat` lints the workspace and links its tests against other FFmpeg builds (D2). The review of that work found 10 more faults; all are fixed, each with a test that fails without its fix where one can show it: from 7.0 FFmpeg's H.264 decoder flags corrupt the frames decoded after a keyframe that isn't IDR, with no recovery point SEI, before it comes out, though their references are all there, and Spherecut took them for damage after a seek and dropped them (the oracle, which now fails any good clip that raises a `Damaged` event, has two such clips and a cut of one, D29); D13, D29 and this file said what FFmpeg's corrupt flag and its HEVC decoder do in ways that weren't so; probe made a PCM track with no channels damage even without samples (D15); a test gated what it expected on libavformat's version; `cargo xtask compat` deleted any directory without its stamp where a target went, the prefix itself when that directory was the prefixes' own, took a prefix whose `.pc` file led to another FFmpeg for found, and ran another pkg-config than the build does; and `build-ffmpeg.sh` said nothing of a step that failed (D2). Its second review found 7 more; 6 are fixed, each with a test that fails without its fix where one can show it, and one is a limit: such a stream with x264's default 3 reference frames never came out unflagged after a seek or at the start of a cut, so a cut played 2 of 32 frames and keyframe seeks went to the file's end (the I-frame decoding starts from is now taken as good, flagged or not, and the oracle has 3-reference clips and a cut, D29); a cut of the 1-B-frame clip cost a second seek after a keyframe seek (the keyframe a seek lands on now counts for the keyframes' lag); x264's intra refresh seeks only to where FFmpeg says decoding has recovered, which isn't always the decode from the start (a limit, D29); `build-ffmpeg.sh` left a half-installed prefix under its name; and compat checked libavcodec alone of the five libraries the build binds, let a target root with `..` build inside a prefix, and checked nothing once the build had run (D2). The review of that work found 5 more; all are fixed, each with a test that fails without its fix where one can show it: an exact seek ended on frames decoded from an I-frame that an MP4 lists as a sync sample but no IDR picture or recovery point vouches for, which may refer to frames before it (x264's I-frames at a flash, keeping 3 references: 28 of 40 exact seeks gave a wrong frame, with no event), so an exact seek in H.264 whose sync samples FFmpeg doesn't flag now decodes from the start of the stream (D29); a Matroska cut at the last keyframe of open-GOP H.264 opened on a leading B-frame decoded without its references (D29); and compat read the build's output where cargo doesn't put it once a build directory or target triple is set, held the libraries it checks to one line of the manifest, and its docs overstated its guard (D2). Its review found 7 more; all are fixed, each with a test that fails without its fix where one can show it: exact seeks still trusted FFmpeg's heuristic at an I-frame whose recovery point SEI counts frames, and 30 of 60 returned a wrong frame as `Exact` (now only a frame FFmpeg flags a keyframe starts an exact run after a seek, and x264's intra refresh, a limit until now, seeks exactly from the start of the stream); a seek back to the start of a cut at an I-frame that isn't IDR, with no B-frames, decoded other pictures than opening did, returned as `Exact` (such a start now decodes on a decoder opened afresh); the docs left the X5's LRV proxies to Pat's next run, where the samples here show them keyed by IDR pictures and seeking as they did (D29); and compat tied its libraries to the workspace's features only in a compat run, didn't count libavresample as FFmpeg's, failed every build in a target root with a brace, and said it recognised FFmpeg layouts it doesn't (D2). `cargo xtask compat` lints the workspace and links its tests against other FFmpeg builds (D2). Slice 4 is the eighth: `spherecut frame` (`crates/cli/src/frame.rs`) with every flag of plan section 5 and `--decoder vulkan`, on the JSON contract as `spherecut.frame/1` with exit statuses 3 and 4 (D27, `docs/cli-json.md`); `core::TimeSpec`, the exact `--time` grammar, with its proptest; `core::fs::TempFile`, moved from `inject`, with `persist_new` (a hard link, which fails with EEXIST whatever turned up at the output, or an exclusive create and a copy where links are refused) beside D16's `persist_replace`; and `cli::interrupt`, the binary's one FFI module, whose SIGINT handler (`SA_RESETHAND | SA_RESTART`) only stores to an atomic that the main thread polls every 10 ms while a worker thread does the work (D28). `--decoder auto` decodes with VA-API on the render node of the GPU that renders and falls back to software, saying why in the result (D39). On lavapipe the Euler table's centre pixels are F (255, 1, 0), R (0, 255, 1), B (1, 0, 255) and L (254, 255, 0), and at the poles the neutral greys 184 (U) and 72 (D) that the f64 reference gives too, since isotropic filtering at the pole blends the marker disc with the grid round it (D32); `--roll 90` keeps F at the centre and puts U on the view's left (amendment 10), shown in a 200° fisheye. The frame code read back through a 360° fisheye is the frame `--time` and `--frame` ask for on a constant-rate MP4 and on its Matroska remux at 1/1000, and `--frame` counts frames on a variable-rate clip. Every error row of the exit table leaves no output and no temporary file; an existing output, a link or a dangling link is never replaced without `--overwrite`, and a file created between the check and the publish is kept (`output_exists`). Ctrl+C 300 ms into a 600-frame exact seek on `long-gop` ends the run about 25 ms later with exit 4 and `cancelled`. The pipeline goldens (D32) store four PNGs, identical here at the build they were blessed on, that `frame --reference` matches at 50.2 to 90.0 dB, and the swscale cross-check is within 1 LSB of swscale and 0.51 to 0.60 LSB of the exact conversion at BT.601 and BT.709, both ranges and both depths, where a swapped matrix or range misses by 18 to 30 LSB. strace shows no IP socket for `frame`. The Tier 2 `vaapi_render_matches_software` and `vulkan_render_matches_software` are written and listed by hw-test, and skip here with their reasons; the same check passes on lavapipe with software standing in for the device and fails on the next frame. Two M1 criteria are ticked: FFmpeg's confinement and the JSON contract. The slice's review found 18 faults; all are fixed or corrected in the docs, each with a test that fails without its fix where one can show it: a frame past a Matroska cut was rendered as the frame asked for (the decoder now finds a Matroska cut from the Segment's size, D13, and `--frame N` on a constant-rate stream checks that the frame shown is N); `--frame N` past a cut on a variable-rate clip failed `decode_failed` rather than by its cause; a Matroska equirect was refused as having no spherical metadata (frame now says it doesn't read Matroska's yet); `--time` and `--frame` values past what the time base holds failed `decode_failed`; negative values got clap's tip; `-o dir/` was refused only after the render; the SIGINT handler replaced an inherited `SIG_IGN` and was installed after the temporary file was made; the Vulkan Tier 2 test skipped on a broken device, passed with clips left out and lacked FFmpeg's words, and no Tier 2 run tried `--decoder auto`; and the tests couldn't tell `--reference` rendering on a GPU, or a crop's sides swapped. The docs' Ctrl+C, output, timing and field lists are corrected, and that lavapipe's `aniso` renders as `iso` is recorded (D13, D16, D27, D39). No golden image changed |
| M2–M9 | Not started |
| Agent integration (track A) | Planned 2026-10-24 at Pat's request (`PLAN.md` track A, DECISIONS D23). The CLI's JSON contract and FFmpeg's confinement are M1 criteria; the confinement is in (D24), and so is the contract for probe and doctor (D27); A1, the MCP server, follows M1's code |

## Environments

### Cloud workspace (where the code is written)

| | |
|---|---|
| OS | Ubuntu 24.04.5 LTS, 4 CPUs, 15 GB RAM |
| Rust | 1.97.0 stable (rustup) with rustfmt, clippy, cargo-deny 0.20.2, cargo-audit 0.22.2; 1.95 for the MSRV check (1.88.0 before slice 3a). clang 18.1.3, strace 6.8 |
| FFmpeg | 6.1.1 (Ubuntu), with -dev headers, libx264, libx265, libdav1d, SVT-AV1. Also built from upstream tags for the compatibility checks in D2: minimal 7.1, 8.1 and 9.0.2; 9.0.2 with VA-API, libx264, libx265 and libdav1d; and 8.1.3 (libavcodec 62.28.103, as on odin and huginn) and 7.1.3 (libavcodec 61.19.101; Fedora 43 has 7.1) with VA-API, Vulkan, libdrm, libx264, libx265, libdav1d, SVT-AV1, Opus, Vorbis and libshaderc, as `scripts/build-ffmpeg.sh` builds them, on Khronos Vulkan-Headers 1.4.354 over the system loader (Ubuntu's 1.3.275 headers are older than 8.1's Vulkan needs). The libdrm, Opus, Vorbis, shaderc and SVT-AV1 -dev packages it needs were installed on 2026-10-24; the 6.1 bindings and link lines built after are byte for byte those built before. No `glslc`, so no SPIR-V compiler at build time |
| Vulkan | lavapipe only: llvmpipe, Mesa 25.2.8, Vulkan 1.4.318 |
| Devices | no `/dev/dri`, no GPU, so no VA-API and no Vulkan Video |
| Network | crates.io, GitHub and Ubuntu apt reachable through a proxy; ffmpeg.org blocked (recorded at setup) |

Here Tier 1 runs fully. `SPHERECUT_TIER2=1 cargo test --workspace
--no-fail-fast -- --ignored` runs the 25 Tier 2 tests.
`goldens_on_host_llvmpipe` passes, since lavapipe is this machine's
llvmpipe, and the others skip: those that need VA-API with "no DRM
render node at /dev/dri/renderD128", the Vulkan Video ones with "no GPU
for Vulkan Video: FFmpeg's Vulkan device is llvmpipe …", the render ones
that need a GPU with "no GPU: Vulkan lists only llvmpipe …",
`vulkan_render_matches_software` with "the only adapter is a CPU:
"llvmpipe (LLVM 20.1.2, 256 bits)"", and,
without `SPHERECUT_SAMPLES`, the samples tests with "samples directory
samples not found". A skip counts as a failed test to cargo, so
the command exits non-zero, and without `--no-fail-fast` cargo stops after
the first test binary. `cargo xtask hw-test --quick` runs them the same
way and reports each one. With `SPHERECUT_SAMPLES` pointing at the
extracted X5 samples, `samples_probe_cleanly` passes.

### Pat's machines

| Machine | Hardware | Software | State |
|---|---|---|---|
| huginn | Ryzen AI Max+ 395, Radeon 8060S (Strix Halo, RDNA 3.5), 128 GB | Fedora 44, kernel 7.0.9, Mesa 26.2.5 (RADV, radeonsi from `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 (Fedora RPM) | `hw-test` at 4cd8a0f, 2026-10-23: 8 passed, 1 skipped. VA-API decodes H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software, and encodes H.264, HEVC and AV1. Vulkan Video decodes AV1 but not H.264 or HEVC ("Device does not support decoding hevc!"). Earlier run at a9128cf: PR #1 |
| odin | RX 7600 (Navi 33, RDNA 3, discrete; the only GPU) | Fedora 44, kernel 7.2.9, Mesa 26.2.5 (RADV and radeonsi from RPM Fusion's `mesa-vulkan-drivers-freeworld` and `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 | `hw-test` at be38fa4, 2026-10-24 (PR #5): 21 passed, 2 failed, both goldens (above). At 67fa1fd (PR #4): 15 passed, 4 failed, all four this side's and fixed after it (the HEVC flush in both GPU seek tests, D29; the download pool's fault limit and fault-counter check, amendment 12). Exact seeks on the GPU take about 0.2 s on 8K30 X5 footage and under 55 ms at p95 on the LRV. Earlier run at 44df411 (PR #3): 12 passed. VA-API and Vulkan Video decode H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software. Only Vulkan Video decodes every X5 mode faster than real time (`docs/perf.md`) |
| Framework 13 Pro | Intel Xe3 | Arch (Omarchy), Hyprland | Arrives late 2026 |

Access: none direct from the cloud workspace. Remote Control on odin is
set up for another project's folder, and a CLI session on huginn wasn't
reachable from here, so hardware runs go through a prompt Pat pastes into
a Claude Code session on the machine, which opens a pull request with the
report (PR #1 for huginn). That session works from a clone of the GitHub
repository, so a commit has to be pushed before it can be run there.

## Samples

- **Insta360 X5**, firmware v1.1.22 (`probe` can't confirm it until the
  trailer is parsed), 7 takes with LRVs, clips 1–4 and 8–10 of
  `docs/SAMPLES.md`: 8K30, 8K25, 5.7K60 (HDR video mode), 4K120, the
  tripod seam clip, the handheld flips and the slow rotation. All 14 video
  files probe cleanly at d2975a6, with no damage, warnings or FFmpeg log
  lines, and are detected as Insta360 dual fisheye or LRV proxy. Findings:
  `docs/formats/insta360.md`, "Pat's X5 samples". Still wanted: I-Log, 360
  audio, FlowState on, a Studio export, a split recording.
- GoPro and DJI: none yet.

## M0 acceptance criteria

| Criterion (`PLAN.md`) | Evidence |
|---|---|
| Environment recorded | The tables and the access note above |
| Opening questions and sample request | Sent 2026-10-22; the sample list is `docs/SAMPLES.md`. Pat answered with Insta360 X5 samples; the rest of the list is still wanted (Samples, above) |
| Workspace | Crates `core`, `sphere`, `media`, `synth`, `testkit`, `cli`, plus `xtask`; all GPL-3.0-or-later. Workspace lints deny `unsafe_op_in_unsafe_fn` and undocumented `unsafe` blocks; `unwrap_used` and `expect_used` warn, allowed only in tests (`clippy.toml`), and clippy runs with `-D warnings` |
| `cargo xtask check` | `xtask/src/check.rs`. `cargo xtask check` passes at 2c8d0c0: rustfmt, clippy, the Tier 1 tests, doc tests, `cargo deny` (advisories, bans, licenses, sources) and `cargo audit` |
| `cargo xtask gen-media` | 12 clips (grid, high-frequency and seam patterns; H.264 and HEVC; 8 and 10-bit; 25, 29.97, 30 and 59.94 fps; stereo and ambiX; with and without Spherical V2; a 4K clip; lossless variants) and 12 broken files: empty, not media, cut at three points, and one for each kind of `moov` damage FFmpeg opens without complaint (chunk offsets, a `trak` type that is not text and one that still is, an `mdhd` timescale, a video codec configuration, an AAC setup and a `proj` type). `rendering_is_deterministic` in `crates/synth/src/lib.rs`; two full runs at c2b2753 gave byte-identical output over all 25 files, the clips, the broken files and `MANIFEST.md` (D9) |
| `spherecut probe` | `crates/cli/tests/probe.rs`, among them `file_names_that_are_not_utf8_are_reported`, `files_named_in_bytes_that_are_not_utf8_are_probed`, `a_codec_ffmpeg_does_not_know_is_a_warning`, `an_audio_setup_ffmpeg_does_not_implement_is_a_warning`, `an_empty_track_ffmpeg_cannot_set_up_is_a_warning`, `ffmpegs_log_is_kept_for_a_file_it_cannot_open` and `a_reader_that_goes_away_is_not_an_error`; the `probe` unit tests on which streams FFmpeg cannot decode are damage (`a_stream_ffmpeg_cannot_set_up_is_damage_only_with_samples_it_reads`, `a_stream_without_a_decoder_in_this_build_is_a_warning`, `a_decoder_that_refuses_the_codec_setup_is_damage_unless_it_lacks_a_feature`); `tags_that_are_not_utf8_come_out_as_valid_json` in `crates/cli/tests/probe_tags.rs`; `a_file_whose_name_is_not_utf8_probes_and_decodes` in `crates/media/tests/non_utf8_names.rs`; `broken_files_give_errors_not_panics` in `crates/synth/tests/generated_media.rs` (every bad file, plus 4000 edits of a real `moov`, of which it must find at least 3000; it found 3453 at 1bfa90b); the `mp4` unit tests `timescales_ffmpeg_cannot_use_are_damage`, `box_types_that_are_not_text_are_damage_where_types_are_registered`, `a_trak_under_another_printable_type_is_damage`, `an_sv3d_ffmpeg_ignores_is_damage` and `zero_types_are_allowed_only_at_the_end_of_a_sample_entry`, and the MP4 fuzzing (D15) |
| `spherecut doctor` | `crates/media/tests/doctor.rs`, `crates/cli/tests/doctor.rs` against lavapipe, and the unit tests in `crates/media/src/doctor.rs`, among them `every_missing_codec_gets_an_issue_with_the_ffmpeg_fix` and `a_driver_that_decodes_wrongly_is_told_apart_from_one_that_refuses`. `crates/media/tests/hw_refusal.rs` checks, on lavapipe, the refusals that doctor's probes share with `VideoDecoder` (D14). Capability checks and fixes: D18 |
| `cargo xtask hw-test` | Unit tests in `xtask/src/hw_test.rs`, among them `paths_that_are_not_utf8_reach_the_tests_unchanged_and_are_reported` and `benchmarks_show_median_range_and_spread`, and `samples_with_a_path_that_is_not_utf8_are_used` in `crates/testkit/src/lib.rs`; the huginn run; the reporting rules in D19 |
| First hardware report | `hw-reports/huginn-a9128cf8df-dirty.md`, then the clean `hw-reports/huginn-4cd8a0f744.md`: toolchain, a Vulkan device (RADV), VA-API HEVC decode and encode working, doctor's decode checks naming exactly what Vulkan Video can't decode, and the benchmarks with their spread (`docs/perf.md`) |
| Docs | README, PLAN, ARCHITECTURE, DECISIONS, PROGRESS, `docs/PROMPT.md`, `docs/SAMPLES.md`, `docs/formats/{insta360,gopro,dji}.md`, `docs/perf.md` |

The M0 review at a9128cf found two blockers (B1, B2) and six should-fixes
(S1 to S6). They are addressed by the merges fa7d370 (hw-test), 8c664d2
(FFI and decoder), c12a4d4 (doctor), 1a3f9c7 (writers) and 4603a56 (MP4
and probe), and by 9300fc0 and e33b77b. The original S1 file (the
`bad-corrupt-moov.mp4` that a9128cf generated) fails `probe`. So does the
file that a9128cf's generator, with the same seed, makes from today's
first clip, which C6 below found still passing.

The final review at d47d7e7 found C1 to C19. They are addressed by the
merges 0cf475e (decoder: C1's library half, C2, C12, C13, C16, C18 and
C19), 15c68b2 (probe: C1's CLI half and C6) and 5b3d02d (doctor, inject
and hw-test: C4, C7 to C10, C14, C15 and C17), and by 721393a (C5 and
C11, the Tier 2 command above). d2975a6 stops `spherecut probe *.mp4 |
head` from panicking when `head` exits. C3, pushing the M0 work, was
done with 2c8d0c0.

Fresh verifiers of those fixes found smaller problems, fixed in the
merges a88d331 (decoded planes stay inside their own plane, the AV1
decoder choice is tested, doctor no longer says wrong output falls back
to the CPU, and Fedora's no-GPU fix drops a useless install line) and
5503e8e (probe opens each stream's decoder, finds renamed and zeroed
boxes, keeps FFmpeg's log lines whole and keeps them when probe fails,
and the samples tests use a directory whose path is not UTF-8). c2b2753
counts a box inside `moov` whose size is 0 as damage. Each branch was
checked by a fresh verifier before it was merged. A last review of the
commits made on main found a closed stderr still panicking (0b479e6,
adead9f), a zeroed `elst` going unnoticed (e93d693, 8462720) and `inject`
writing after a QuickTime terminator (324e224), all fixed with tests.

huginn's rerun at 4cd8a0f (PR #2, `hw-reports/huginn-4cd8a0f744.md`) came
back clean, so M0 is done.

## Resumed 2026-10-28

- `m1/frame` (slice 4, `spherecut frame`) is finished, reviewed and
  merged.
- `m1/compat-fixes` is merged: the second review of the FFmpeg 8.1.3
  work found that good H.264 without recovery-point SEI raised `Damaged`
  after seeks on FFmpeg 7.1 and later (its corrupt flag comes from
  recovery state, not a lost reference), and that SEI-less H.264 keeping
  3 references could seek to the wrong keyframe; that a zero-channel PCM
  stream became damage instead of a warning; and that `cargo xtask
  compat` could delete a directory it hadn't made and report a pass
  against the system FFmpeg. All are fixed with tests, and a third
  review checked the fixes (D2, D13, D15, D29). An x264 intra-refresh
  stream still sought only to the frame FFmpeg calls recovered, until
  `m1/h264-landing` (below).
- `m1/h264-landing` is merged: the review of that fix found exact seeks that
  returned wrong frames as `Exact` in H.264 whose `stss` lists I-frames
  with no recovery point that later frames refer across, a Matroska cut
  that opened on a leading B-frame, and three faults in compat's build
  checks. All are fixed with tests (D2, D29). Exact seeks in such H.264
  now decode from the start of the stream; the X5's LRV proxies aren't
  such streams (their keyframes are IDR pictures), and their exact seeks
  cost what they did. Its review found that exact seeks still trusted
  FFmpeg's heuristic at I-frames whose recovery point counts frames, and
  that a seek back to the start of a cut at an I-frame that isn't IDR
  decoded other pictures than opening did, both returned as `Exact`, and
  five nits in compat and the docs; all are fixed, with tests where one
  can show it (D2, D29). x264's intra refresh now seeks exactly too,
  from the start of the stream; so does a stream keeping one reference
  whose I-frames that aren't IDR are listed (a remux), which was right
  and now decodes about 2.8 times the frames (D29).

## M1 criteria ticked

Ticked in `PLAN.md` with slice 4, whose acceptance (m1-plan section 14)
meets them in full. Neither depends on a GPU or a hardware decoder: both
are about what FFmpeg may open and what the CLI prints.

| Criterion (`PLAN.md` M1) | Evidence |
|---|---|
| FFmpeg opens only the file Spherecut opened | Slice 1 (D24): `ffi::confine` (our own AVIO, the `io_open` refusal, the demuxer allow-list, `protocol_whitelist`, `format_whitelist`, `max_pixels`) under probe, the decoders and doctor; the root `clippy.toml`'s `std::net` ban; `crates/cli/tests/confinement.rs` (the HLS playlist naming a file outside its folder, ffconcat, an `http://` segment and the path `http://127.0.0.1:1/x.mp4` refused with the reason, files named like protocols read as files); probe refusing other containers by name (the `lrc` test in `probe.rs`, the README, D15); strace a prerequisite of `cargo xtask check`. Slice 4 adds `frame_opens_no_network_socket`: `spherecut frame clip.mp4 --adapter cpu -o out.png` under `strace -f` opens no `AF_INET` or `AF_INET6` socket and connects only to Unix sockets, next to `probe_opens_no_network_socket_and_no_listed_file` |
| The CLI's JSON contract (probe, doctor, frame) | `crates/cli/tests/contract.rs`: one compact document per run on success and failure, a panic and a usage error included (`a_panic_is_one_internal_document_and_exit_1`, `usage_errors_under_json_are_a_document_and_exit_2`, `stdout_carries_only_the_document_whatever_the_log_says`); schema and version in `schema`, `ok`, stable codes listed literally and documented (`every_code_is_listed_and_stable`, `the_document_describes_every_code_schema_and_status`); one shape for one file or many; times as `{exact, seconds}` plus `frame` (`times_and_rationals_read_back_exactly`, and frame's result in `frame_documents_have_the_envelope_too`); exit statuses 0, 1 and 2 for probe and doctor (doctor's errors exit 1), and 0 to 4 for frame: `frame_exits_0_to_3_with_one_document_each` (no Vulkan driver through `VK_DRIVER_FILES`, `--decoder vaapi --render-node /nonexistent`) and `sigint_during_a_seek_cancels_with_exit_4`; `only_frame_handles_sigint` (probe and doctor keep the default action) and `an_ignored_sigint_stays_ignored` (an inherited `SIG_IGN` is kept). `--overwrite`: `an_existing_output_is_kept_unless_overwrite` and `a_race_for_the_output_is_lost_without_replacing` in `frame.rs`, and core's `persist_new` tests |

## odin's third run and slice 4's checks

Checks of `m1/frame` on 2026-10-28, on code unchanged since ff97714 (the
merge of main; only docs changed after it): `cargo xtask check` passed
three times, the last two in a row on the branch's last commit;
`TimeSpec`'s proptests passed at `PROPTEST_CASES=100000` (2 s); `cargo
+1.95 build --workspace --all-targets` built with no warning (3 min 38
s); and `cargo xtask compat` passed clippy and the link against FFmpeg
8.1.3 (libavcodec 62.28.103, as Pat's) and 9.0.2, and Tier 1 against
8.1.3, the new frame tests included (8 min 12 s). `cargo xtask hw-test`
lists the two new Tier 2 tests.

Checks of main at 498d6e1, the merge of all three branches: `cargo
xtask check` passed, and `cargo xtask compat` passed clippy, the link
and the binding against FFmpeg 8.1.3 and 9.0.2, and Tier 1 against
8.1.3. A last review of the decoder reopen and the landing rule
(hardware paths, the merge's interactions, exact seeks) found no wrong
frame and three minor faults, fixed on `m1/seek-nits`, each with a test
that fails without its fix (D29, D30):

- A file trimmed by stream copy mid-GOP (an edit list that hides its
  first IDR picture's frame) reopens the decoder at every seek that
  lands on its first packet, though a flush gives the same frames:
  20 to 25% more time on those seeks, measured in software. Fixed in
  1748d3a: the first packet's NAL units say whether its picture is one
  FFmpeg flags a keyframe; the new test's first clip reopened 5 times in
  5 seeks without the fix, and none with it (99f2d6b counts reopens in
  `DecodeStats`).
- A reopen that fails (in practice only out of memory) loses the
  stream's first packet and is taken for the file's damage. Fixed in
  4289173: on a device it is the device's failure, and the fallback goes
  on in software; in software it is an error until a seek. Injected,
  it had made an exact seek to frame 2 return frame 6 as `Exact`.
- When the landing rule turns down a landing, damage met while decoding
  to its first frame is said again by the decode from the earlier
  landing: two `Damaged` events, and two warnings in `spherecut frame`,
  for one damaged packet. Fixed in 62e3be4: damage is said once from one
  seek to the next, however often decoding meets it, which also ends two
  older doubles: a keyframe seek past a cut said the cut and playing on
  to it said it again, and the step after a stopped seek said playback's
  damage again.

Those tests found that the oracle fails a closed-GOP clip behind an edit
list of 3 frames, H.264 or HEVC, at frame 13: an exact seek to a
keyframe right after a keyframe seek there seeks again where decoding
on was cheaper. Its frames are right, and it was so before the fixes
(D29).

odin's third run (PR #5, `hw-reports/odin-be38fa409e.md`, at be38fa4):
21 of 23 Tier 2 tests pass, including all four that failed in PR #4. Two
golden tests fail, and the fixes are next:

- `goldens_on_gpu_match_reference` on the RX 7600:
  - `fisheye_360` is 44.67 dB against the CPU reference, below its 46.59
    dB floor, which was calibrated on lavapipe (measured − 3 dB).
    `fisheye_180` (47.78) and `stereo_tinyplanet` (51.45) are lower on
    RADV too, while `rect_forward` is higher (58.5). Only the wide,
    minified views lose, which points at the hardware's filtering
    precision (bilinear weights and the mip blend), not a bug. Next:
    give hardware its own floor, measured on RADV and recorded in D32,
    never below 36 dB, after checking in Pat's diff images that the
    error sits where many mip levels blend.
  - `passthrough_exact`: 11 limited-range codes (74, 86, 129, 135, 141,
    153, 159, 208, 214, 220 and 226) come out one code low, where the
    model of the chain predicts only 86, 159 and 165, and three of them
    are outside its f16 bound. The likely cause is RADV's colour export
    to an 8-bit target, which packs the shader's output to f16 with
    round-toward-zero before the unorm conversion. Next: have the encode
    pass round to codes itself and write integers (an `Rgba8Uint` target
    or a storage buffer), so every driver gives the same codes, then
    re-bless through the gate.
- `goldens_on_host_llvmpipe` (LLVM 22.1.8, Mesa 26.2.5): `fisheye_180`
  differs from the PNG blessed on LLVM 20.1.2 by at most 5 LSB, one over
  the other-driver rule's 4, at 62.79 dB and SSIM 0.99994. Next: replace
  the single-pixel maximum with a bound on how many pixels may pass 4
  LSB (and a hard maximum), measured on both builds and recorded in D32.

## Known issues and limits

- Spherecut opens only MP4/MOV and Matroska/WebM files; anything else,
  WAV, FLAC or MP3 music beds included, needs a DECISIONS entry that
  widens the demuxer list (D24). Within those containers FFmpeg may run
  any decoder it has (no `codec_whitelist`), each limited to 2^27 pixels
  per picture, which doesn't bound how many pictures a decoder keeps. A
  stream over the limit is a probe warning and a `VideoDecoder` error
  that give its size, except one whose frames say more than its
  container and whose size FFmpeg takes from the container (MJPEG, for
  one): that fails while decoding, as FFmpeg decides.
- The backend's hardware path has run here only on lavapipe, which
  decodes nothing, and on a software stand-in for the device (media's
  `fault-injection`). Its Tier 2 tests are written and skip here; H1 is
  their first run on a GPU. Every audio open is still
  `not_implemented` (slice 6).
- A fallback to software is said by a `tracing::warn!`, the `FellBack`
  event and each frame's `DecodePath`, and in `spherecut frame` by its
  result's `decode.fallback` and its text line, not as one of its
  `warnings` (D39). The overlay's decoder line and badge and the GUI
  notice with doctor's fix come with the overlay and the GUI (D30).
- `spherecut frame --decoder auto` tries VA-API and never Vulkan Video
  (D39); elsewhere a caller asks for a kind. Slice 5 picks it from
  doctor's checks (m1-plan amendment 9), and `auto` should follow.
- Only `spherecut frame` asks wgpu's adapter for its GPU's render node
  (D30's lookup by bus id, then PCI ids). doctor has no adapter yet, so
  it tests `/dev/dri/renderD128` unless `--render-node` names another; on
  a machine with two GPUs the two can differ.
- `spherecut frame` here: every run renders on lavapipe or the CPU
  reference and decodes in software (`auto` with no render node), so
  `hw_unsupported` and `hw_failed` as errors, `auto`'s fallback after a
  device refused or failed, and `anisotropy_unavailable` (lavapipe has
  anisotropic filtering) have no Tier 1 test; the Tier 2
  `vaapi_render_matches_software` and `vulkan_render_matches_software`
  are written and skip here, and H1 is their first run. They hold the
  hardware decode to software, and `auto` to VA-API on the node found
  from the rendering adapter's ids (D39), but they expect the device to
  decode, so `auto`'s fallback after a device refusal or failure and
  `anisotropy_unavailable` have no test of frame's in any tier yet.
  lavapipe's `--filter aniso` renders byte for byte what `iso` does
  (Mesa 25.2.8, on views that squeeze the source), so there the
  result's `"aniso"` changes nothing and nothing tells the two apart
  (D27, Next). The first Ctrl+C stops a seek or a read at once, but
  probe, the GPU's set-up, the render and the PNG's encode take no stop
  token, so during them it takes effect when the step ends, seconds
  later at a large `--size`. A frame started with SIGINT ignored keeps
  it ignored. The race test
  stages the file that appears at the output with a debug-only switch,
  `SPHERECUT_DEBUG_FRAME_RACE`; `persist_new`'s copy fallback is tested
  through a hook that refuses the link, since no filesystem here lacks
  hard links. A second Ctrl+C kills the run at once and may leave its
  `.spherecut-….tmp` beside the output. `-o /dev/null` and other
  non-files are refused (D16). The Euler table's pole rows can't be
  white and black at the centre pixel: there isotropic filtering at the
  analytic LOD blends the marker disc with the grid to grey (184 and 72
  here, as the f64 reference gives), so they assert a neutral centre and
  `the_markers_round_a_fisheye_turn_with_the_camera` checks the markers
  round the poles (D32). `--frame N` on a variable-rate stream decodes
  every frame from the first, so it slows the further in N is.
  `sigint_during_a_seek_cancels_with_exit_4` doubles `long-gop` to 1200
  frames when a 600-frame seek takes under 600 ms, which costs it about
  10 s more; here that is common, not the exception (the seek took
  0.6 to 0.8 s in slice 4's review, and the test 7.2 to 19.5 s).
- Matroska/WebM: frame refuses every Matroska file as `not_equirect`
  unless `--equirect` is given, since Spherecut doesn't read Matroska's
  spherical metadata (Spherical Video V2's `Projection` element) yet;
  probe doesn't report it either. A cut is found from the Segment's
  size (D13), so a Matroska file whose Segment's size is unknown (a live
  recording) that is cut isn't seen as cut: a `--time` past its end
  renders the last frame, as for an uncut file; `--frame N` there is
  checked against the constant rate and refused. Near a cut, frame
  also refuses a few frames the file still holds: frames the decoder
  drains after the failed read may follow frames the cut lost, so a
  seek whose next frame is one of them is `truncated` (D29).
- `vaapi_download_pool_reuses_buffers` and its Vulkan twin check the
  pool by the buffers it allocates, none after the warm-up, and report
  the decoding thread's minor faults per frame without a limit: on
  odin, VA-API's driver took 116 per 3840x1920 NV12 frame and 227 per
  P010 frame mapping each surface to read it, and at that size the
  faults can't show a pool that doesn't work, since glibc reuses freed
  frames' memory (D30, m1-plan amendment 12). FFmpeg's VA-API download
  creates, fills and destroys a frame-sized VAImage per frame, a second
  copy and, above 32 MiB (5760x2880 P010, 8K NV12), a fresh mapping per
  frame; the tests now report a 5760x2880 P010 clip to measure it, and
  slice 5 checks mapping the surface directly instead (D30).
- On lavapipe with FFmpeg 6.1, H.264 and HEVC are refused as `Withdrawn`
  (FFmpeg has Vulkan hwaccels for them, which can't start without a video
  queue), not `NotOffered`; only a codec without a Vulkan hwaccel, such
  as MPEG-4 Part 2, is `NotOffered` (D30).
- A device failure on a damaged packet is `HwFailed`, so the damage shows
  only once software decodes the packet after the fallback. A mid-stream
  fallback reopens the file and decodes from a keyframe.
- Damage is said once from one seek to the next by what tells it apart
  (D29): a packet with neither a byte position nor a timestamp, or a
  run of frames referring to a lost picture without a pts, is said each
  time decoding meets it. A trimmed HEVC stream that starts at a picture
  that isn't IRAP still decodes its start on a decoder opened afresh.
- `vaapi_seek_matches_software` and its Vulkan twin decode every sample
  stream from start to end on the device and make 50 exact seeks in each
  (20 above 2880², such as the X5's 8K takes) in software and on the
  device: an estimated 5 to 10 minutes each on huginn or odin for Pat's
  X5 set, most of it the software seeks at 8K. The noisy
  5.7K clips they use are 2 s, made the first time by the tests at about
  200 Mbps and cached in the target's `tmp`; hw-test's 20 s clips are
  slice 5's. Their four cropped clips, two of them cropped on the left
  through FFmpeg's `h264_metadata` and `hevc_metadata` filters, are the
  only streams with a left crop: none of the X5 samples has one.
- Frames of a stream cropped on the left are now cropped exactly in
  software too, where slice 2a kept FFmpeg's aligned cropping, so their
  planes may start off a 32-byte boundary; swscale, converting one,
  warns once that this is slower (D30).
- Exact seeks in a stream whose timestamps go backwards decode it twice
  from the start, and such a stream is found out only once its
  timestamps are seen going backwards. The path for frames without
  timestamps can't be reached through MP4 or Matroska, so it has no
  test. A target among the leading pictures of the next keyframe seeks
  rather than decoding on. A Matroska file's constant rate is checked on
  its first 64 blocks and its Cues only. The new run that catching up
  starts after it skipped frames is a guard no test reaches. Resuming in
  a backwards stream after a frame handed out before the stream was found
  out takes the first frame at its pts for it, and starts a run (D29).
- The renderer and its goldens are checked on lavapipe only (Mesa
  25.2.8, llvmpipe with LLVM 20.1.2): their PSNR floors are that
  driver's, each the measured PSNR less 3 dB (less 1 dB against the
  ideal), and another Mesa may need them recalibrated. lavapipe
  truncates an f32 stored into an f16 target, so the shaders round every
  f16 store to the nearest half themselves (D31). The anisotropic filter
  is built and validated but not compared with anything; the
  implicit-derivative debug mode is the goldens' seam and pole control.
  The render Tier 2 tests are written but haven't run on a GPU, so
  whether the staging belt should replace `write_texture` (the triggers
  `bench_upload_5k7` reports) and how near a GPU comes to lavapipe's
  floors are unmeasured; H1 runs them on huginn and odin. A lost device
  makes every render entry point return `gpu_failed`; recovering from it
  (a new device, the slots uploaded again) is slice 5's. 16-bit sources lose their low 4 bits in the f16 planes, and a 360°
  frame whose width isn't a multiple of its chroma subsampling (odd-width
  4:2:0, which AV1 and VP9 allow) shows a half-texel chroma step at the
  ±180° seam (D31).
- The stored goldens match exactly only at the Mesa build they were
  blessed on. After a Mesa update here they are held by the other-driver
  rule (PSNR ≥ 48 dB, SSIM ≥ 0.995, at most 4 LSB) until blessed again,
  and a blessing writes its day and commit into the manifest, so blessing
  again changes the manifest though no image does. lavapipe filters 8-bit
  planes to whole codes, so `range_full_vs_limited` and
  `siting_left_center` assert the plan's 2 and 1 LSB at 10 bits and only
  record NV12's 4 and 1.99. The ideal comparisons rest on the pixels 2.5
  levels down or more (2600 of the tiny planet's 65536, and 10044 and
  28532 in the shimmer rows); milder minification is compared with the
  CPU reference only. `shimmer_fisheye360`'s control has the least room:
  level 0 changes 3.26 times as much as the ideal, against a bound of 3
  (D32). A blessing holds each floor to the one blessed before it, within
  0.5 dB, not to the first, so blessings that each lower a floor a
  little can walk it down; the manifest's diff shows each step. The CPU
  comparison shares render's colour fold, so the colour rows' other
  truths and `color.rs`'s unit tests are what check the fold itself.
- Slice 4 adds about 28 s to a warm `cargo xtask check` here, the sum
  of what its tests take (76.4 s for the whole check at ff97714, before
  its review, without cargo-deny and cargo-audit). Its two new test
  binaries take 13.8 s (`frame.rs`, of which
  `a_frame_lost_to_a_cut_is_never_shown_in_its_place` is 7.2 s alone,
  most of it encoding its 150-frame clip, `every_failure_leaves_nothing_behind`
  4.9 s and `pipeline_goldens` 4.2 s) and 2.6 s (`tier2_frame.rs`'s
  Tier 1 stand-in), and `contract.rs` goes from 1.4 s without its frame
  tests to 12 to 17 s with them, nearly all of it
  `sigint_during_a_seek_cancels_with_exit_4`, which doubles `long-gop`
  more often than not here (above): it took 7.2 s when it didn't and
  15.0 to 19.5 s when it did in the review, and the binary 12.6 s in
  the last run; `an_ignored_sigint_stays_ignored` runs beside it. The
  strace test for frame and core's new unit tests run beside the others
  in their binaries and add under 0.2 s. The slices' numbers below were
  measured on other days, and this box's timings vary by up to a third
  between runs (render's `goldens.rs` took 10.5 s in this check against
  7.2 to 7.4 s in slice 3b's).
- Slice 3b adds about 8 s to a warm `cargo xtask check` (42.2 s against
  34.3 s at 09342c6, both without cargo-deny and cargo-audit): the
  goldens' test binary takes 7.0 s on lavapipe, `goldens` itself 6.9 s
  of it, and its two shimmer rows 4.5 s of that (each renders 16 views
  and supersamples 8 ideal ones at 16×16 a pixel). Building render and
  testkit at opt-level 2 in dev profiles keeps it there: at 0 the
  goldens took 16.9 s. The adapters test and the bench's smoke run take
  under a second. The fixes after its review add about 2 s more (41.9 s
  against 39.6 s with 642ccb4's render and synth, run alternately on the
  same day, two runs each): the goldens' binary takes 7.2 to 7.4 s
  against 6.8 to 6.9, since the harness's own tests now bless their rows
  into scratch directories, and the upload tests 1.7 s against 0.8, for
  the bench's block test on a pair of 3840² frames and the 16-bit ingest.
- Slice 3a adds about 2.5 s to a warm `cargo xtask check` (33.6 s
  against 31.1 s at ef1a9d1, both without cargo-deny and cargo-audit,
  which here depend on the advisory database; 33.2 s before its first
  review's fixes): the render tests' four binaries take 2.4 s on
  lavapipe (1.8 s before the review's tests), and the new sphere and
  testkit tests the rest.
  A cold check also builds wgpu and its tree once, about a minute here.
- Slice 2a adds about 12 s to `cargo xtask check`, nearly all of it the
  seek oracle's test binary: 11.7 s of a 26 s warm check here (8.9 s
  before the second review's fixes added the 25-frame open-GOP, RADL,
  spliced and resumed clips, 4.6 s before the first review's added the
  tied, backwards, variable-rate Matroska, long Matroska and catch-up
  ones). Every exact seek in a backwards clip decodes it twice from the
  start, and the oracle seeks four times in each frame's interval. Slice
  2b adds about 5 s more: the fallback tests (3.9 s here), which fail
  every frame of two clips, and the Tier 1 checks of the Tier 2 checks.
- FFmpeg 6.1's H.264 decoder, left to drop the frames it decodes from
  references a seek skipped, loses the frames still in its reorder buffer
  at the end of the stream, and doesn't flag those it drains then.
  Spherecut has it output them, drops the flagged ones and, after a seek,
  every leading picture of the keyframe it landed on, by pts (D29);
  FFmpeg 7.1 to 9.0 have not been checked for the bug.
- swscale's conversion is tested end to end only for UYVY and RGB24, the
  formats a raw MOV carries; the others by format (D30).
- x265 (3.5 here, and its master) leaves the dts of the frames within
  its B-frame delay undefined when a clip ends inside that delay: one
  frame, or two with a B-pyramid, gets its pts minus whatever x265's
  heap held. FFmpeg 6.1's mov muxer writes it; 8.1.3's refused it in
  about half of the runs of `pipeline_goldens` against 8.1.3 in slice
  4's review ("pts/dts pair unsupported", `AVERROR_PATCHWELCOME`), in
  its one-frame lossless clips. synth now writes HEVC clips of at most
  `X265_BFRAME_DELAY` (2) frames, and frame's tests their one-picture
  clip, without B-frames (`writers.rs`
  `hevc_clips_within_x265s_bframe_delay_have_no_b_frames`); the
  pictures, and so the goldens, are the same. gen-media's
  `hifreq-hevc-lossless` and `seam-hevc-lossless` (two frames) change
  bytes but not pixels.
- synth's clips are tagged centre-sited chroma, which is what their
  4:2:0 is, so probe's JSON says `center` for them, `gen-media`'s set
  included.
- `VideoDecoder` copies hardware frames to system memory. Zero-copy
  (VA-API to DMA-BUF to wgpu) is proved in M1, and whether it becomes the
  default is decided before M3 (D25).
- `VideoDecoder` finds a Matroska cut by the Segment's size, as `probe`
  does, but not a cut in a Matroska file whose Segment's size is unknown
  (a live recording), nor a fragmented MP4 cut between fragments, which
  reads as a shorter file (D13, D15).
- Without decoding, `probe` can't find damage inside `mdat`, or damage in
  `moov` that keeps the structure consistent, such as changed sample
  sizes or times (`stts`, `ctts`, `elst`), chunk offsets that still point
  into `mdat`, or changed bits of a parameter set. Bytes of `mdat` that no
  chunk uses are deliberately not damage. Of 55 generated files corrupted
  the way a9128cf made `bad-corrupt-moov.mp4` (64 byte flips in the second
  half), 16 still pass `probe` at 8435b8b, as at 9d212c3. Their reports
  match the clean file's, apart from FFmpeg's decode errors in two, one
  byte of a handler name in one, and in one a display matrix that rotates
  by 0 degrees (D15).
- The decoder's invalid-data skip path has no test.
- Some tests and the encode paths still call ffmpeg-next accessors that
  build C enums (D11).
- `inject` and `Mp4Writer` replace only a regular file, so an output that
  is a directory, FIFO, device or socket is refused: `-o /dev/null` and
  writing to a pipe aren't supported, and would need a non-atomic path of
  their own (D16). They check the output before any work and again just
  before the rename, but no rename call replaces only a regular file, so
  a pipe or device created at the output in the microseconds between the
  last check and the rename would still be replaced.
- `inject` refuses damaged input: anything `Mp4Tree::damage()` finds,
  which includes the box reader's warnings. When `moov` comes before
  `mdat`, it also refuses edits that take `moov` past the box reader's
  limit (`MAX_BOXES`, 250 000 boxes), found when it re-reads `moov` to
  shift the chunk offsets. With `moov` at the end of the file no offsets
  move and there is no re-read, so such edits give a valid MP4 that
  Spherecut's own box reader stops partway through, and `probe` reports
  as damaged. No chunk offset is ever wrong.
- `hw-test` runs the `spherecut doctor` it built through a lossy UTF-8
  copy of the binary's path, so a cargo target directory whose path isn't
  UTF-8 makes that run fail; the report shows the failure.
- The Insta360 trailer is detected but not parsed, so `probe` shows no
  model or firmware.
- GoPro and DJI detection comes from format research only; no samples
  have been checked.
- Fedora's `ffmpeg-free` lacks libx264 and libx265, which `gen-media` and
  the Tier 1 media tests need. Use RPM Fusion's `ffmpeg`. Tier 1 also needs
  libdav1d, an FFmpeg built with Vulkan, Mesa's lavapipe and strace.
  Against a 9.0.2 built without Vulkan, every Tier 1 test passes except
  those that need FFmpeg's Vulkan device: `crates/cli/tests/doctor.rs`
  and `crates/media/tests/hw_refusal.rs` (D2).
- `cargo xtask check` builds only against the FFmpeg pkg-config finds,
  unless `SPHERECUT_FFMPEG_COMPAT` lists other prefixes, and a fresh
  checkout has none: other builds are checked only where someone has
  built them (`scripts/build-ffmpeg.sh`) and set the variable. Here the
  check ran without it, and `cargo xtask compat` was run against 7.1,
  7.1.3, 8.1.3 and 9.0.2 by hand (D2). compat links the tests against each FFmpeg but runs them against one only, so
  a behaviour that differs between versions is found only on that one.
- An exact seek in H.264 whose sync samples FFmpeg doesn't flag
  keyframes as frames (I-frames that aren't IDR, with no recovery point
  SEI or one that counts frames, and x264's intra-refresh recovery
  points on P-frames) decodes from the start of the stream (D29):
  nothing in the file says the frames after them don't refer across
  them. It is right, and slow in a long file; keyframe seeks aren't
  affected. The X5's LRV proxies aren't such streams: their keyframes
  are IDR pictures, and their exact seeks cost what they did.
- On FFmpeg 7.1.x, HEVC damage that the decoder skips (a slice header or
  frame start it can't decode) is silent: no error, no frame flagged
  corrupt, so no `Damaged` event, where 6.1 and 8.0 and later raise one
  (D29). Fedora 43 ships 7.1.
- `VideoDecoder::open`, the M0 decoder the media tests and Tier 2 checks
  use, doesn't ask FFmpeg for corrupt frames
  (`AV_CODEC_FLAG_OUTPUT_CORRUPT`), and without that flag FFmpeg 8.0 and
  later refuses the HEVC pictures that refer to a damaged reference
  picture, skipping them with "Skipping invalid undecodable NALU" up to
  the next IRAP picture: there the damage loses those frames without an
  error. The backend's decoder, which everything above media uses, asks
  for them and reports it (D13, D29).
- With every seek oracle test opened on one thread, as hardware decoders
  run, all of them pass on 6.1 and on 8.1.3 but
  `an_edit_list_hides_its_lead_in_and_clip_time_starts_after_it`, whose
  cost check fails on both: an exact seek to frame 10 of `edit.mp4`,
  whose keyframe is at or before the target, seeks (once, decoding 7
  frames) where the check expects it to decode forward. The frames it
  returned until then were right. Tier 1 runs the oracle on frame
  threads, and on one thread only
  `on_one_thread_no_frame_from_before_a_seek_comes_out_after_it` (D29).
- `hw-test`'s prerequisite check still judges Fedora by installed packages
  (`ffmpeg-free`, `mesa-va-drivers-freeworld`), unlike doctor (D18).
- `hw-test`'s `vainfo` and prerequisite checks look only at
  `/dev/dri/renderD128`.
- Doctor's Vulkan checks run on FFmpeg's default Vulkan device, while the
  Tier 2 Vulkan Video tests use the GPU behind VA-API's render node
  (`open_vulkan_device`). On a machine with two GPUs they can differ;
  both name the device (D38).
- Doctor doesn't print the full VA-API profile list; the hw-test report
  includes `vainfo`.
- ambiX AAC plays as 4.0 surround in players that ignore `SA3D` (D17).
- On huginn, VA-API decode of the X5 8K clips varies a lot between runs
  (1.2 to 2.9 times real time; `docs/perf.md`), which hw-test marks as
  unstable. The copy of each frame to RAM is a likely part of it; the M1
  performance overlay will show how much.
- On odin, VA-API with its copy to RAM is below real time at X5 5.7K60 and
  8K30 and slower than software on the synthetic clips, while Vulkan Video
  keeps up; huginn's unified memory doesn't show this (`docs/perf.md`).
  Slice 5b measures zero-copy for both paths (m1-plan amendment 9).
- huginn has no Vulkan Video H.264 or HEVC until it gets RPM Fusion's
  `mesa-vulkan-drivers-freeworld`, as odin has (Pat's call).
- No Intel machine until the Framework arrives.
- No GUI yet, so no GUI smoke run (M1).
- The JSON contract's doctor error path (an issue of severity error,
  `doctor_errors`, exit 1) is tested on a constructed report, and the
  binary only for `ok` matching its exit status: this workspace's FFmpeg
  has every decoder doctor requires. doctor's issues have no codes of
  their own, and the envelope's `warnings` is always empty for probe and
  doctor; their findings are in `result` (D27).
- `spherecut probe` with no file now exits 2 (a usage error) where M0
  exited 1, and probe's `--json` shape changed from M0's (an object for
  one file, an array for several) to the envelope; `docs/cli-json.md` has
  the new one.
- In the contract, probe checks a Matroska file only for its length, so
  one damaged inside that FFmpeg can't open is `open_failed`, with
  FFmpeg's log, rather than `damaged`. An MP4's `full` check covers
  `moov`'s sample tables but not a fragmented file's `moof`/`trun`, so
  fragments whose samples lie past the end of the file pass unnoticed. A panic under `--json` is an
  `internal` document (exit 1); a crash by signal, such as a fault
  inside FFmpeg, still leaves stdout empty. The unreadable-file test
  runs the binary as nobody through `setpriv` when the tests run as
  root, with a readable copy beside the file to show that nobody reaches
  the directory; only probe's unit test covers it where neither works,
  or where nobody can't reach the target directory.

## Next

1. Slice 4's merge, once its review's fixes (18 findings, on
   `m1/frame`) are checked, and `m1/compat-fixes`' checks and review; the golden fixes odin's third
   run calls for (above); then worker threads and the frame cache (slice
   5), audio, then the eframe GUI, which hands eframe render's own device
   through egui-wgpu's `WgpuSetup::Existing` rather than the plan's
   `CreateNew`, so that slice 5b's Vulkan Video device reaches the viewer
   (D31). H1, after slice 5, runs slice 2b's, 3b's and 4's Tier 2 tests
   on huginn and odin: `bench_upload_5k7` decides whether the staging
   belt replaces `write_texture`, `goldens_on_gpu_match_reference`
   whether the golden floors need a GPU's own entries (D32), and
   `vaapi_render_matches_software` and `vulkan_render_matches_software`
   hold `spherecut frame`'s hardware decode to its software decode, and
   `--decoder auto` to VA-API on the node found from the rendering
   adapter (D39). Still with no test in any tier, for H1 or later:
   `auto`'s fallback after a device that turns a stream down or fails
   on a frame, `anisotropy_unavailable`, and whether `--filter aniso`
   does anything: a Tier 2 check on RADV or ANV that a grazing view
   with `aniso` differs from `iso`, or is closer to `render_view_ideal`,
   belongs with render's Tier 2 at H1 or the GUI's slice (slice 7),
   since lavapipe's `aniso` renders as `iso` does (D27). Reading
   Matroska's spherical metadata (Spherical Video V2's `Projection`
   element, which FFmpeg gives as side data) in probe, so that frame
   takes a tagged Matroska equirect without `--equirect`, waits for a
   slice that needs Matroska sources.
2. The Insta360 trailer reader (metadata, calibration, IMU), needed from M2.
3. Agent integration A1 (the MCP server) once M1's code is in, while
   M1's hardware report is pending (Pat chose this order on 2026-10-24).

## How to run

```sh
cargo xtask check                 # everything Tier 1 (add --fix to apply rustfmt)
cargo xtask gen-media             # synthetic media into test-media/
cargo test -p spherecut-synth --test seek   # the seek oracle (D29)
cargo test -p spherecut-synth --test fallback   # hardware fallback, with fault injection (D30)
cargo test -p spherecut-render    # the render core on lavapipe against its CPU reference (D31)
cargo test -p spherecut-render --test goldens   # the golden images (D32)
SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test goldens   # bless them, after a change meant to move them
SPHERECUT_UPDATE_GOLDENS=lower cargo test -p spherecut-render --test goldens   # bless, letting floors drop by more than 0.5 dB (D32)
cargo run -p spherecut-cli -- probe <file>... [--json]   # --json: one document, docs/cli-json.md
cargo run -p spherecut-cli -- doctor [--json]
cargo run -p spherecut-cli -- frame <file> -o out.png [--time 12.5 | --frame 370] [--yaw 30 --pitch -10 --roll 0] [--fov 90] [--json]   # README, "Rendering a frame"
cargo test -p spherecut-cli --test frame   # frame end to end, with the pipeline goldens (D32)
SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test frame pipeline_goldens   # bless the pipeline goldens
cargo xtask hw-test               # on a real machine; report in hw-reports/
scripts/build-ffmpeg.sh n8.1.3 ~/ffmpeg   # an FFmpeg release from its tag into ~/ffmpeg/n8.1.3-full (about 4 min on 4 CPUs)
cargo xtask compat ~/ffmpeg/n7.1.3-full ~/ffmpeg/n8.1.3-full --test ~/ffmpeg/n8.1.3-full   # clippy and a link against each (about 3 min and 1.4 GB a prefix on 4 CPUs), Tier 1 against one (D2); targets are spherecut-compat-<label> in $SPHERECUT_FFMPEG_COMPAT_TARGET_DIR or target/compat/, and --clean deletes them after
SPHERECUT_FFMPEG_COMPAT=~/ffmpeg/n8.1.3-full cargo xtask check   # check lints and links against those too
```
