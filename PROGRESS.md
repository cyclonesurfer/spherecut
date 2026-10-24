# Progress

Updated 2026-10-24, with FFmpeg confined to the file Spherecut opened (D24) and the CLI's JSON contract for probe and doctor (D27). Milestone criteria are in `PLAN.md`;
decisions and their reasons in `DECISIONS.md`.

## Status

| Milestone | State |
|---|---|
| M0 Foundation | **Done** (2026-10-23). The huginn report at 4cd8a0f is clean: every Tier 2 test passes but one, which skips because RADV on Strix Halo has no Vulkan Video H.264 or HEVC decode. Evidence below |
| M1 Viewer | In progress. `VideoDecoder` (software, VA-API, Vulkan) is its first slice. FFmpeg's confinement (D24) is the second; its criterion stays unticked until `spherecut frame`, which it also covers, exists and has its strace test. The CLI's JSON contract for `probe` and `doctor` (D27, `docs/cli-json.md`) is the third; its criterion stays unticked until `frame` joins it, with exit statuses 3 and 4, SIGINT and `--overwrite` |
| M2–M9 | Not started |
| Agent integration (track A) | Planned 2026-10-24 at Pat's request (`PLAN.md` track A, DECISIONS D23). The CLI's JSON contract and FFmpeg's confinement are M1 criteria; the confinement is in (D24), and so is the contract for probe and doctor (D27); A1, the MCP server, follows M1's code |

## Environments

### Cloud workspace (where the code is written)

| | |
|---|---|
| OS | Ubuntu 24.04.5 LTS, 4 CPUs, 15 GB RAM |
| Rust | 1.97.0 stable (rustup) with rustfmt, clippy, cargo-deny 0.20.2, cargo-audit 0.22.2; 1.88.0 for the MSRV check. clang 18.1.3, strace 6.8 |
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
| huginn | Ryzen AI Max+ 395, Radeon 8060S (Strix Halo, RDNA 3.5), 128 GB | Fedora 44, kernel 7.0.9, Mesa 26.2.5 (RADV, radeonsi from `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 (Fedora RPM) | `hw-test` at 4cd8a0f, 2026-10-23: 8 passed, 1 skipped. VA-API decodes H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software, and encodes H.264, HEVC and AV1. Vulkan Video decodes AV1 but not H.264 or HEVC ("Device does not support decoding hevc!"). Earlier run at a9128cf: PR #1 |
| odin | RX 7600 (RDNA 3) | Fedora | No report yet |
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
- Doctor doesn't print the full VA-API profile list; the hw-test report
  includes `vainfo`.
- ambiX AAC plays as 4.0 surround in players that ignore `SA3D` (D17).
- On huginn, VA-API decode of the X5 8K clips varies a lot between runs
  (1.2 to 2.9 times real time; `docs/perf.md`), which hw-test marks as
  unstable. The copy of each frame to RAM is a likely part of it; the M1
  performance overlay will show how much.
- No odin report yet, and no Intel machine until the Framework arrives.
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
  FFmpeg's log, rather than `damaged`. A panic under `--json` is an
  `internal` document (exit 1); a crash by signal, such as a fault
  inside FFmpeg, still leaves stdout empty. The unreadable-file test
  runs the binary as nobody through `setpriv` when the tests run as
  root, and only probe's unit test covers it where neither works.

## Next

1. The first `cargo xtask hw-test` run on odin (any time; the prompt is
   the one huginn used).
2. M1, now that FFmpeg is confined (D24): the `MediaBackend` trait
   around `VideoDecoder` (seek, audio, worker threads, frame cache), the
   wgpu projection shader and `spherecut frame` on the CLI's JSON
   contract (D27; with exit statuses 3 and 4, SIGINT and `--overwrite`)
   and with its strace test, golden images on lavapipe, then the
   eframe GUI. Bump `rust-version` to 1.95 with the GUI crates (D1).
3. The Insta360 trailer reader (metadata, calibration, IMU), needed from M2.
4. Agent integration A1 (the MCP server) once M1's code is in, while
   M1's hardware report is pending (Pat chose this order on 2026-10-24).

## How to run

```sh
cargo xtask check                 # everything Tier 1 (add --fix to apply rustfmt)
cargo xtask gen-media             # synthetic media into test-media/
cargo run -p spherecut-cli -- probe <file>... [--json]   # --json: one document, docs/cli-json.md
cargo run -p spherecut-cli -- doctor [--json]
cargo xtask hw-test               # on a real machine; report in hw-reports/
```
