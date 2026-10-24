# Progress

Updated 2026-10-24, with FFmpeg confined to the file Spherecut opened (D24), the CLI's JSON contract for probe and doctor (D27), odin's first hardware report, M1 slice 2a: `MediaBackend` with frame-exact seek in software, native pixel layouts and the colour defaults (D29, D30), slice 2b: hardware decoding through the backend, with fallback to software, the pooled download, the render node lookup and the Tier 2 seek tests (D30), and slice 3a: the render core, checked on lavapipe against its CPU reference (D31). Milestone criteria are in `PLAN.md`;
decisions and their reasons in `DECISIONS.md`.

## Status

| Milestone | State |
|---|---|
| M0 Foundation | **Done** (2026-10-23). The huginn report at 4cd8a0f is clean: every Tier 2 test passes but one, which skips because RADV on Strix Halo has no Vulkan Video H.264 or HEVC decode. Evidence below |
| M1 Viewer | In progress. `VideoDecoder` (software, VA-API, Vulkan) is its first slice. FFmpeg's confinement (D24) is the second; its criterion stays unticked until `spherecut frame`, which it also covers, exists and has its strace test. The CLI's JSON contract for `probe` and `doctor` (D27, `docs/cli-json.md`) is the third; its criterion stays unticked until `frame` joins it, with exit statuses 3 and 4, SIGINT and `--overwrite`. Slice 2a is the fourth: `core::video` and `core::cancel`, `media::backend` with `FfmpegBackend` and the frame-exact seek of plan section 3.3 in software (D29), native pixel layouts, swscale for the rest and the colour default rules (D30), synth's new clips and its `SyntheticBackend` and `GatedBackend`. The seek oracle (`crates/synth/tests/seek.rs`) is exact on every clip of the set, among them clips whose timestamps tie or go backwards and variable-rate Matroska files with a constant default duration; it checks what each seek cost and every run link, and its negative controls miss. The slice's review found 16 faults (wrong frames returned as `Exact` after a cut, in broken timestamps and in variable-rate Matroska, false run links when catching up, a duration missing for Matroska, an index trusted before Cues are read); all are fixed, each with a test that fails without its fix where one can show it (D29, D30). Its second review found 10 more (an open-GOP H.264 leading picture and a RADL picture returned after a seek, seeks past an MP4 cut, an MP4 spliced by its edit list read as constant-rate, resuming after a stopped seek in tied and backwards streams, an FFmpeg log capture that lost its lines to another thread, a test that a change in load could fail, and an overflow on a far-off Cues entry); all are fixed too, with tests where one can show it (D29). Slice 2b is the fifth: `DecodeMode::Hardware` decodes through the backend on VA-API or Vulkan Video, on one device per kind per backend opened on the render node `media::hw` finds (bus id, then PCI ids, then `renderD128`; `--render-node` overrides it), single-threaded, with frames kept on the device until handed out and downloaded into a pool of 256-byte-aligned buffers. `Fallback::Software` opens in software when the device can't be opened or turns the stream down, and reopens in software when it fails mid-stream, going on from the frame after the last one returned with no gap and no repeat; `Fallback::Fail` returns the error. A device's refusal or failure is its own error (`HwUnsupported`, the new `HwFailed`), never the file's damage. Media's `fault-injection` feature, turned on only by synth's tests, fails any frame as the device would, and the Tier 1 fallback tests (`crates/synth/tests/fallback.rs`) fail every frame of H.264 and HEVC clips while seeking, catching up and playing; they also fall back on lavapipe and on a render node that isn't there. The Tier 2 tests the plan names are written and listed (`vaapi_seek_matches_software`, `vaapi_colorinfo_matches_software`, `vaapi_download_pool_reuses_buffers`, `bench_seek_latency`, and Vulkan Video versions of the first three), and here they skip with their reasons; their checks pass on software standing in for a device and fail on one that fails. The slice's review found 4 faults (a stream cropped on the left came off the device narrower than software's frame, or failed on its first download at 10 bits, since FFmpeg's aligned cropping keeps columns by the pixel format's own rule; the pool tests couldn't tell a pool from a fresh buffer per frame; a device that won't open was named `vaapi`, and the Vulkan device's log line named the node asked rather than the GPU taken); all are fixed, with tests where one can show it. Both paths now crop exactly as the stream says, and the Tier 2 seek and colour sets have clips cropped on the left (D30). Slice 2 is done once 2b merges; its hardware results come with H1. Slice 3a is the sixth: `spherecut-render` on wgpu 30.0.1 (Vulkan only) with `rust-version` 1.95 (D1): the device (`Gpu`, `AdapterChoice`, `GpuCaps`, `RenderError` with contract codes), canonical plane textures (deep planes through an integer scratch and an ingest pass into f16) with area mips capped near 64 px, GPU slots, the `Uploader` (`write_texture` or a staging belt, one submit per frame), the projection shader (every projection and morphs, the shader's own LOD, colour from `ColorInfo`), the encode pass, `render_still` and the f64 CPU reference (D31), with sphere's `pixel`, `orient`, `uv_gradient` and `project_morph` and testkit's `compare` and `fnv64`. On lavapipe `render_still` matches the reference at 49.6 to 59.0 dB through every projection and ten plane layouts at 8 to 12 bits, both ranges, three matrices and three sitings, and the mips match an independent area mean; negative controls tell level 0 from trilinear and one siting from another (`crates/render/tests/`). Slice 3b adds the goldens and the Tier 2 tests. No M1 criterion is ticked yet |
| M2–M9 | Not started |
| Agent integration (track A) | Planned 2026-10-24 at Pat's request (`PLAN.md` track A, DECISIONS D23). The CLI's JSON contract and FFmpeg's confinement are M1 criteria; the confinement is in (D24), and so is the contract for probe and doctor (D27); A1, the MCP server, follows M1's code |

## Environments

### Cloud workspace (where the code is written)

| | |
|---|---|
| OS | Ubuntu 24.04.5 LTS, 4 CPUs, 15 GB RAM |
| Rust | 1.97.0 stable (rustup) with rustfmt, clippy, cargo-deny 0.20.2, cargo-audit 0.22.2; 1.95 for the MSRV check (1.88.0 before slice 3a). clang 18.1.3, strace 6.8 |
| FFmpeg | 6.1.1 (Ubuntu), with -dev headers, libx264, libx265, libdav1d, SVT-AV1. Also 7.1, 8.1 and 9.0.2 built from upstream tags, for the compatibility checks in D2 |
| Vulkan | lavapipe only: llvmpipe, Mesa 25.2.8, Vulkan 1.4.318 |
| Devices | no `/dev/dri`, no GPU, so no VA-API and no Vulkan Video |
| Network | crates.io, GitHub and Ubuntu apt reachable through a proxy; ffmpeg.org blocked (recorded at setup) |

Here Tier 1 runs fully. `SPHERECUT_TIER2=1 cargo test --workspace
--no-fail-fast -- --ignored` runs the 19 Tier 2 tests and all of them
skip: those that need VA-API with "no DRM render node at
/dev/dri/renderD128", the Vulkan Video ones with "no GPU for Vulkan
Video: FFmpeg's Vulkan device is llvmpipe …", and, without
`SPHERECUT_SAMPLES`, the samples tests with "samples directory samples
not found". A skip counts as a failed test to cargo, so
the command exits non-zero, and without `--no-fail-fast` cargo stops after
the first test binary. `cargo xtask hw-test --quick` runs them the same
way and reports each one. With `SPHERECUT_SAMPLES` pointing at the
extracted X5 samples, `samples_probe_cleanly` passes.

### Pat's machines

| Machine | Hardware | Software | State |
|---|---|---|---|
| huginn | Ryzen AI Max+ 395, Radeon 8060S (Strix Halo, RDNA 3.5), 128 GB | Fedora 44, kernel 7.0.9, Mesa 26.2.5 (RADV, radeonsi from `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 (Fedora RPM) | `hw-test` at 4cd8a0f, 2026-10-23: 8 passed, 1 skipped. VA-API decodes H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software, and encodes H.264, HEVC and AV1. Vulkan Video decodes AV1 but not H.264 or HEVC ("Device does not support decoding hevc!"). Earlier run at a9128cf: PR #1 |
| odin | RX 7600 (Navi 33, RDNA 3, discrete; the only GPU) | Fedora 44, kernel 7.2.9, Mesa 26.2.5 (RADV and radeonsi from RPM Fusion's `mesa-vulkan-drivers-freeworld` and `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 | `hw-test` at 44df411, 2026-10-24 (PR #3): 12 passed, 0 skipped. VA-API and Vulkan Video decode H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software. Only Vulkan Video decodes every X5 mode faster than real time (`docs/perf.md`) |
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
  `spherecut frame` has no strace test yet, because it doesn't exist.
- The backend's hardware path has run here only on lavapipe, which
  decodes nothing, and on a software stand-in for the device (media's
  `fault-injection`). Its Tier 2 tests are written and skip here; H1 is
  their first run on a GPU. Every audio open is still
  `not_implemented` (slice 6).
- A fallback to software is said only by a `tracing::warn!`, the
  `FellBack` event and each frame's `DecodePath`. The overlay's decoder
  line and badge, the GUI notice with doctor's fix and `frame`'s stderr
  note and JSON warning come with the overlay, the GUI and `frame` (D30).
- Nothing yet picks between VA-API and Vulkan Video: a caller asks for a
  kind. Slice 5 picks it from doctor's checks (m1-plan amendment 9).
- No GPU adapter is known before slice 3, so the render node lookup
  gives `/dev/dri/renderD128` unless `--render-node` names another.
- `vaapi_download_pool_reuses_buffers` and its Vulkan twin check the
  pool by the buffers it allocates, none after the warm-up. Their limit
  of 50 minor faults per frame counts everything the decoding thread
  does, the driver's own faults included: a driver that maps each
  surface to read it, as VA-API's download does, may go over it whatever
  the pool does, and the note's allocation count then says the faults
  are the driver's. At 3840x1920 the faults can't show a pool that
  doesn't work, since glibc reuses freed frames' memory (D30).
- On lavapipe with FFmpeg 6.1, H.264 and HEVC are refused as `Withdrawn`
  (FFmpeg has Vulkan hwaccels for them, which can't start without a video
  queue), not `NotOffered`; only a codec without a Vulkan hwaccel, such
  as MPEG-4 Part 2, is `NotOffered` (D30).
- A device failure on a damaged packet is `HwFailed`, so the damage shows
  only once software decodes the packet after the fallback, and a
  `Damaged` event can then come twice when a seek is done again. A
  mid-stream fallback reopens the file and decodes from a keyframe.
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
- Slice 3a's renderer is checked on lavapipe only (Mesa 25.2.8,
  llvmpipe with LLVM 20.1.2): its PSNR floors are that driver's, each the
  measured PSNR less 3 dB, and another Mesa may need them recalibrated.
  lavapipe truncates an f32 stored into an f16 target, so the shaders
  round every f16 store to the nearest half themselves (D31).
  The anisotropic filter and the implicit-derivative debug mode are
  built and validated but not compared with anything; the goldens'
  negative controls (slice 3b) use the latter. `bench_upload_5k7` is
  slice 3b's, so whether the staging belt should replace
  `write_texture` is unmeasured on a GPU; the smoke run here (640×320,
  4 frames) shows the allocator's block count steady on both paths.
  A lost device makes every render entry point return `gpu_failed`;
  recovering from it (a new device, the slots uploaded again) is slice
  5's. 16-bit sources lose their low 4 bits in the f16 planes, and a 360°
  frame whose width isn't a multiple of its chroma subsampling (odd-width
  4:2:0, which AV1 and VP9 allow) shows a half-texel chroma step at the
  ±180° seam (D31).
- The M1 plan's Euler table for `frame`'s tests (section 12.1) says
  `--roll 90` puts the U marker on the right edge's side; with the plan's own
  convention, q = R_y(−yaw) · R_x(pitch) · R_z(−roll) with roll > 0
  clockwise, which `sphere::orient` implements and tests, world up then
  lies on the view's left. Slice 4 has to settle which is meant before
  that golden is blessed.
- Slice 3a adds about 2 s to a warm `cargo xtask check` (33.2 s against
  31.1 s at ef1a9d1, both without cargo-deny and cargo-audit, which here
  depend on the advisory database): the render tests' three binaries
  take 1.8 s on lavapipe, and the new sphere and testkit tests the rest.
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
- synth's clips are tagged centre-sited chroma, which is what their
  4:2:0 is, so probe's JSON says `center` for them, `gen-media`'s set
  included.
- `crates/cli/tests/confinement.rs` leaves its `confinement-<pid>-strace`
  directory in the target's `tmp` after each run.
- `VideoDecoder` copies hardware frames to system memory. Zero-copy
  (VA-API to DMA-BUF to wgpu) is proved in M1, and whether it becomes the
  default is decided before M3 (D25).
- `VideoDecoder` doesn't detect Matroska cuts or most fragmented-MP4 cuts,
  and `probe` checks Matroska only against its Segment length (D13, D15).
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

1. Slice 3b: the goldens on lavapipe (the manifest, blessing, the
   level-0 and implicit-LOD negative controls and the supersampled ideal,
   D32), synth's `render_view_ideal`, the Tier 2 render tests and
   `bench_upload_5k7`, on the API slice 3a left for them. 3b has to
   revise `passthrough_exact`'s limited-range criterion first: through
   the f16 linear target Y = 86, 159 and 165 come out one code from
   round((k − 16) · 255 / 219) even with every store rounded to nearest
   (D31). Then wgpu's
   adapter gives the render node lookup its GPU; `spherecut frame` on the
   CLI's JSON contract (D27; with exit statuses 3 and 4, SIGINT and
   `--overwrite`) and its strace test; worker threads and the frame
   cache, audio, then the eframe GUI, which hands eframe render's own
   device through egui-wgpu's `WgpuSetup::Existing` rather than the plan's
   `CreateNew`, so that slice 5b's Vulkan Video device reaches the viewer
   (D31). H1, after slice 5, runs slice 2b's
   Tier 2 tests on huginn and odin.
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
cargo run -p spherecut-cli -- probe <file>... [--json]   # --json: one document, docs/cli-json.md
cargo run -p spherecut-cli -- doctor [--json]
cargo xtask hw-test               # on a real machine; report in hw-reports/
```
