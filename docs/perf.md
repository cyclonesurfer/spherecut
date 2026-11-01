# Performance

Measured numbers per machine, so regressions show up. Each row comes from a
`cargo xtask hw-test` report (release build); the report's commit is in the
table. Numbers from the GPU-less cloud workspace are not recorded here.

## Targets (from `docs/PROMPT.md`)

| Target | Measured by |
|---|---|
| The UI stays at display refresh even when decode falls behind | GUI smoke run frame times (from M1) |
| 5.7K30 raw or stitched footage plays in real time with hardware decode on odin and huginn | `bench_playback_realtime_5k7_8bit` and `_10bit` (the gate, M1 slice 5), `bench_playback_stitched_samples`, `bench_samples_realtime_decode`, `bench_decode_hevc_4k_and_5k7`; the GUI smoke run from M1 slice 8 |
| 8K30 plays in real time, at least on proxies | `bench_playback_x5_raw` and `bench_playback_realtime_8k_8bit` (recorded, M3's budget), and the decode rows on 8K samples and their LRV proxies |
| A random seek shows a frame in under ~150 ms on proxies | `bench_seek_latency` (M1 slice 2b), on the X5's LRV proxy among others |
| Reframed 4K30 exports at or above real time with hardware encode on odin and huginn | export benchmark (M2) |
| Frame caches are bounded, sized from available memory, configurable | cache tests (M1) |

## How the benchmarks work

Benchmarks are Tier 2 tests whose names start with `bench_`. Each one
calls `spherecut_testkit::bench::measure(name, unit, f)`, which runs `f`
once to warm up (file cache, GPU clocks), then `SPHERECUT_BENCH_RUNS` times
(default 3), and records the median, the minimum, the maximum and the
number of runs. `bench::record(name, value, unit)` records a single value
as one run.

Under `cargo xtask hw-test`, `SPHERECUT_BENCH_OUT` points at
`<cargo target dir>/hw-bench.tsv`. The target directory comes from
`cargo metadata`, so it follows `CARGO_TARGET_DIR` and `build.target-dir`.
hw-test deletes the file before the tests run, so it holds only this run's
numbers, and passes its path to the tests byte for byte. Each line is
`name`, `median`, `unit`, `min`, `max`, `runs`, tab-separated, with the
numbers at full precision. Outside hw-test the same line goes to stderr as
`BENCH ...`. Tier 2 tests run one at a time (`--test-threads=1`), so
benchmarks don't compete with other tests.

`bench::try_measure_latency(name, unit, batch)` is for latencies: `batch`
returns a batch of them (a seek's, say), and runs once to warm up, then
`SPHERECUT_BENCH_RUNS` times. It records two rows, `<name>: median` and
`<name>: p95`, each the median over the runs of that batch's median or
95th percentile (by nearest rank), with their range. So a row's spread
says how much the batches differ, not how much one seek differs from
another; the p95 row says that.

`bench::try_measure` is `measure` for a path that may fail, such as
Vulkan Video on a driver that can't decode the file: if the warm-up or
a run fails, nothing is recorded and the benchmark gets the error.
`bench::note(text)` and `bench::not_measured(name, reason)` write a line
that starts with `# ` instead, collapsed to one line, which the report
lists under its table as "Notes from the benchmarks". That is where a
row that has no result says why.

The report's Benchmarks table has the columns Benchmark, Median, Range,
Runs and Spread. hw-test rounds the numbers to two decimals only for the
table, and works out Spread from the unrounded ones: (max − min) / median,
as a whole percentage. Over 10% the row is marked **unstable**, which
usually means the machine was busy or changing clocks. Spread shows "n/a"
when the median is 0 or less, and Range and Spread are empty for a single
run.

`--quick` builds the workspace in debug mode and leaves the benchmarks out
(`--skip bench_`): they appear in the Tier 2 table as "skipped: benchmark,
left out by `--quick`", and the Benchmarks section says it was skipped.

Current benchmarks:

- **`bench_decode_hevc_4k_and_5k7`**: decodes all 60 frames of a synthetic
  3840×1920 and 5760×2880 HEVC 8-bit clip, in software, on VA-API and on
  Vulkan Video, in frames per second. The VA-API and Vulkan Video numbers
  include copying each frame to system memory, which the render path will
  avoid later.
- **`bench_samples_realtime_decode`**: for each recording mode in
  `samples/`, takes the longest file, decodes up to ten seconds of all its
  video streams at once, one thread per stream as playback does, and
  reports the slowest stream as a multiple of real time (1.0 = just keeps
  up). An Insta360 X5 8K30 file has two 3840² streams, so 1.0 means 60
  decoded 3840² frames per second. It does so in software, on VA-API and
  on Vulkan Video; both hardware rows include the copy to system memory.

- **`bench_seek_latency`**: how long `VideoDecode::seek` takes to hand
  out its frame, through `FfmpegBackend` as the player seeks, in
  milliseconds: exact and keyframe seeks, in software, on VA-API and on
  Vulkan Video, on the noisy 5760×2880 HEVC 8-bit clip (2 s, open GOPs
  of 30 with B-frames, about 200 Mbps), the longest X5 8K `.insv` (its
  first 3840² stream) and the longest LRV proxy. A batch is 20 seeks to
  random times on a decoder opened for it, the same times in every
  batch; the rows are `<mode> seek, <clip>, <path>: median` and `: p95`.
  Exact seeks decode from the keyframe before the time, or forward from
  where the decoder is, so they vary with the GOP; keyframe seeks decode
  one frame. Hardware rows include the copy of the one frame returned to
  system memory: frames decoded on the way stay on the device. The
  target above is the LRV's exact p95.
- **`bench_upload_5k7`** (M1 slice 3b, DECISIONS D31): what putting a
  decoded frame on the GPU costs, on the GPU `SPHERECUT_ADAPTER` names or
  the first one Vulkan lists, for 8- and 10-bit 5760×2880 frames (NV12
  and P010) and an 8- and a 10-bit pair of 3840² frames (the X5's 8K is
  two such streams), with rows as the pooled download lays a decoded
  frame out (each rounded up to 256 bytes). For each input and each
  `Uploader` path (`WriteTexture`, the default, and `StagingBelt`), over
  300 steps of uploading the input and waiting for it: `upload <input>,
  <path>: CPU per step` (the `upload` call, in ms), `…: GPU ingest and
  mips` (`per frame` for a pair), and `…: steps that made or freed
  allocator blocks`, from the allocator's report taken while each step
  is in flight and once it is done, with a note of the block count's
  range and the most memory reserved and allocated. The same rows for
  `upload <input>, <path>, 2 in flight` come from steps that only poll,
  two in flight, as the upload thread runs (no GPU row). Then, on the
  5.7K slots, `mips <input>: GPU` and `project <input>, rectilinear 90° |
  tiny planet 300° to 1920x1080 | 3840x2160: GPU`, each the mean of 60
  passes. Each row is a run's mean, recorded as the median and range of
  `SPHERECUT_BENCH_RUNS` runs after a warm-up. A note for each 5.7K
  input says whether the CPU trigger holds (the belt's CPU time under
  0.9 times `write_texture`'s, as H1 restated the plan's "above 1.5
  times, or above 4 ms", whose 4 ms arm held on odin where the belt
  cost the same), and a note for each input whether `write_texture`
  makes or frees allocator blocks within a step, waited or in flight,
  the other trigger. Either holding on huginn or odin makes the belt the
  default (D31); neither has on odin, through H1 (below). A last note names
  the GPU and how long the bench took. A device without timestamps
  inside encoders has no GPU rows, and a note says so. It needs at most
  about 1 GB on the GPU.

- **The playback benchmarks** (M1 slice 5, `crates/playback/tests/tier2_playback.rs`,
  DECISIONS D34): a file played through the engine's `Player` on the
  GPU, decoding on its render node, with `present` and `snapshot` at
  60 Hz deadlines on a thread marked as the UI thread and each new frame
  projected into an offscreen 1920×1080 target (the X5's two lenses side
  by side, 960×1080 each). Each run has an unthrottled pass (a
  `ManualClock` moved to the next frame as soon as it is on screen) and
  a real-time pass (a `SystemClock`, looping at the end). Rows, under
  `<clip>, <decoder>` (the decoder `VA-API (Auto)`, `Vulkan (Auto)` or
  `Vulkan (Auto, zero copy)` for the one `Auto` picks, else `VA-API`,
  `Vulkan` or `software`):
  `decoded per second, unthrottled`, `decoded per second, real time` and
  `shown per second, real time`, each one run's full seconds (Runs is
  the seconds, Range the slowest and the fastest second, and the spread
  says how even it was); `dropped after 1 s`, `decode-dropped after 1 s`
  and `late presents after 1 s` (a present whose frame had already
  ended). A note per run gives its loops, the UI ticks it missed, each
  stream's decode path and every stage's p50/p95 in ms (`decode`,
  `download`, `convert`, `upload_cpu`, `upload_gpu`, `render_cpu`,
  `render_gpu`, `ui_frame`, `shown_interval`, `av_offset`).
  - `bench_playback_realtime_5k7_8bit` and `_10bit` play the 5.7K perf
    clips (below) and **gate**: on `Auto`'s hardware as the app plays
    it by default (D42), 0 dropped and 0 decode-dropped after the first
    second of 60 s, every unthrottled second at 1.2 × 29.97 = 35.96 fps
    or more, and every stream on that hardware throughout. Where `Auto`
    picks Vulkan Video and the device can be shared, the gated run keeps
    the frames on the GPU (path B), is named `Vulkan (Auto, zero copy)`
    with a note of the frames taken there and refused, and fails if any
    was refused; `Auto`'s copy path is then recorded as `Vulkan (Auto)`.
    The other hardware kind and software run 20 s and only record.
  - `bench_playback_realtime_8k_8bit`: the 8K perf clip, recorded.
  - `bench_playback_x5_raw`: the longest X5 file of each mode, both
    lenses as two streams, on each decoder, recorded (M3's 8K budget).
    Then each two-track perf clip (below, from M3.1), both tracks
    opened as a sample's are, unthrottled only, for up to 30 s:
    `perf clip <name>, <decoder>: decoded per second, unthrottled` on
    `Auto`'s hardware as the app plays it by default, and again as
    `VA-API (Auto, SPHERECUT_ZERO_COPY=1)` where `Auto` picks VA-API and
    the GPU imports DMA-BUFs (path A), with a note of each run's frames,
    decode paths and stages. A note compares path A's median second with
    the copy's, for D25's revisit (M3 plan 4.3's K2), and another gives
    K2's engine half from `pair-hevc8-2880-2997-noisy-20s`: decided on
    `VA-API (Auto)` with its frames copied, and a pass when every full
    second decodes 36 fps a lens or more, on VA-API throughout, without
    a stall. It plays whichever of the samples and the pairs are there,
    and a stall fails it.
  - `bench_x5_8k30_zero_copy_vs_copy` (slice 5b, D25): each X5 mode
    on VA-API with the copy, VA-API with zero copy (frames imported as
    DMA-BUFs) and Vulkan Video with the copy, recorded with the
    `upload_cpu` and `upload_gpu` p50 and p99 beside the rows above,
    and a note of the frames imported and the imports refused. It skips
    where the GPU can't import, or an import was refused, saying why.
  - `bench_playback_stitched_samples`: each stitched equirect sample,
    gated as the perf clips are when it is 5.7K at 30 fps or less; skips
    while there is none.
  - `bench_perf_clips_software_decode`: `perf clip <name>: software
    decode`, in fps, which the report's "Perf clips" section shows,
    flagged **too easy** over 150 fps; a two-track clip a lens at a
    time, `perf clip <name> lens <k>: software decode`, so that each
    track is flagged on its own.
- **`bench_export_pair_decode`** (M3.1, M3 plan 4.3's P-DEC,
  `crates/export/tests/tier2_pair_decode.rs`): the export's pair decode
  as section 12 runs it, two `HwOutput::Download` decoders on a
  dual-fisheye file's lens tracks, `serial` (one thread steps both) and
  `a thread a lens`, on `VA-API download`, `Vulkan Video download` and
  `software`, over the X5 8K30 clip 9 (`handheld_flips`), the 8K25,
  5.7K60 and 4K120 samples, and the three two-track perf clips. Each
  lens holds at most the 5 frames the export will. Rows, under
  `pair decode <clip>, <path>, <setup>`: `slower lens beside the encode`
  or `slower lens alone` (fps a lens: the frames after the first over
  the time from both lenses' first frame), `opening` (ms) and
  `peak RSS growth beside the encode` or `alone` (MiB), with
  `the encode beside` (fps) on the gated rows. The gated rows, clip 9
  (K1 and K4) and `pair-hevc8-2880-2997-noisy-20s` (K2's export half),
  decode 600 frames a lens beside an `h264_vaapi` encode of a fixed
  3840×2160 frame at 30 fps, once to warm up and then three times; the
  rest decode up to 600 frames once, alone. For K3 each path also
  decodes clip 9 alone, as a pair and as lens 0 by itself
  (`…, <setup>, for K3` and `…, lens 0 only, for K3`). In these runs
  the consumer lets a pair go only once every channel is full, as a
  render slower than the decode would, so that all 10 frames of the
  pair (5 of lens 0) are alive at once; the other rows' consumer takes
  each pair as it comes, and holds fewer once the decode is the slow
  side. K3 decides only when each of its runs had all its frames alive
  at once, and its note gives that peak and the bytes it accounts for
  beside the growth. Every run is a child process of its own, so that
  its peak RSS growth is its own. A note per K gives its verdict by the
  plan's rule: Pass, the redirect the plan names, or not decided and
  why. A verdict never fails the test; a run that fails does, and with
  no hardware row at clip 9 it skips. On odin it should take about 25
  minutes, most of it software's gated rows.
- **The perf clips** (DECISIONS D37): hw-test makes them once, with
  libx265 at an average bitrate, into `<cargo target dir>/hw-media/`:
  `hevc8-5k7-30fps-noisy-20s.mp4` and `hevc10-5k7-30fps-noisy-20s.mp4`
  (5760×2880, 100 Mbps) and `hevc8-8k-30fps-noisy-20s.mp4` (7680×3840,
  120 Mbps), 20 s of synth's grid with new noise on every frame, open
  GOPs of 30 with three B-frames. `--quick` makes none, and the
  benchmarks that play them skip.
- **The two-track perf clips** (M3 plan 7.2, from M3.1): synth's
  `perf_pairs`, two square lens tracks in one MP4, each coded alone by
  threaded x265 with its corner codes and its own noise, 600 frames a
  lens, specified as the X5 codes each mode where x265 can: closed
  GOPs, no B-frames, one-pass ABR, 8-bit full range BT.709.
  `pair-hevc8-3840-2997-noisy-20s.mp4` (2×3840² at 29.97, an IDR every
  60 frames, 65 Mbps a lens), `pair-hevc8-2880-5994-noisy-10s.mp4`
  (2×2880² at 59.94, an IDR every 120, 80 Mbps) and
  `pair-hevc8-2880-2997-noisy-20s.mp4` (2×2880² at 29.97, an IDR every
  60, 80 Mbps). What x265 makes of that was measured on 192² copies of
  121 frames a lens, coded in the cloud by its x265 3.5, not on the
  full clips: every picture but the IDRs (IDR_N_LP) a TRAIL_R reference
  P frame, at 59.94 too, and one slice a picture with WPP on. The X5
  codes every other frame at 59.94 as a non-reference TSA_N, which x265
  3.5 gives only to B-frames, which are reordered; and the masters'
  slice headers, read in the cloud, show 10 slices at 8K and 5 at 5.7K
  with WPP off, and IDR_W_RADL, which x265 3.5 can't match (it refuses
  several slices without WPP, and writes IDR_W_RADL only before RADL
  pictures, which need B-frames). hw-test's Tier 1 codes the 192²
  copies again on each machine with its own x265 and checks their IDR
  spacing and P-only coding, though not which IDR type they take nor
  their slices (synth's `pairs`, which also prints, with `--nocapture`,
  what that x265 does when asked for temporal layers: x265 4.x, as odin
  and huginn link it, is unverified until one of them runs it). The
  report then reads each lens of the full clips that machine made or
  found: its bitrate from its own samples, its software decode, and its
  coding, its IDR interval from `stss` and its pictures by NAL type
  ("IDR every 60; TRAIL_R 590, IDR_N_LP 10"), in bold where a picture is
  neither an IDR nor a TRAIL_R or an IDR is off the interval, and the
  59.94 pair's rows say it is P only where the X5 codes TSA_N. Nothing
  reads the full clips' slices. The report's x265 row gives
  `pkg-config`'s version of the headers where there is one, and the
  version that the info SEI of a frame hw-test codes names, which is the
  x265 FFmpeg links: odin's `pkg-config` has no x265 file, so its row
  read "headers not found" before and gives the SEI's version alone
  now. A pair's stamp holds that version and the corner codes' layout
  (`synth::code::FORMAT`), so a pair is made again when either changes.

Not benchmarks, but their numbers are in the same notes:
`list_adapters_and_features` notes each Vulkan adapter's info (with its
PCI bus id and timestamp period), the features of the formats the
renderer uses, and its features, downlevel flags and limits;
`goldens_on_gpu_match_reference` notes each golden row's numbers on each
GPU, which say how near it comes to lavapipe's floors, with each CPU
check's error by LOD band, and `goldens_on_host_llvmpipe` the stored
images' drift on the machine's own llvmpipe, with the pixels past 4
LSB (D32). The
`*_download_pool_reuses_buffers` tests note the minor page faults per
downloaded frame on the decoding thread, per frame and per MB, for
3840×1920 NV12 and P010 and 5760×2880 P010, after 30 frames to warm up,
with the kernel's transparent huge page setting, and how many buffers
the download pool allocated in the warm-up and after it (D30). A pool
that works allocates a few in the warm-up and none after, which the
tests check; the faults are reported, not limited (m1-plan amendment
12). At 3840×1920 glibc hands a freed frame's 11 or 22 MB back from its
heap, so the faults there are the driver's: on odin, VA-API's 116 and
227 a frame, about 10 per MB. At 5760×2880 P010 a frame is 50 MB, over
what glibc keeps, so a fresh frame-sized block per frame, such as the
VAImage FFmpeg's VA-API download creates for each frame, shows as
thousands of faults a frame; transparent huge pages set to always hide
most of them.

How the Vulkan Video rows read:

- They are named `decode <size> HEVC 8-bit, Vulkan Video (incl. copy to
  RAM)`, `<mode>: Vulkan Video decode` and `<mode> seek, <clip>, Vulkan
  Video: …`, next to the VA-API rows of the same clip or mode, in the
  same unit, so the two hardware paths compare row for row.
- They run on the GPU behind VA-API's render node, or the first GPU if
  no Vulkan device is listed as that one (DECISIONS D38). A note under
  the table names it, for example "Vulkan Video rows ran on AMD Radeon RX
  7600 (RADV NAVI33) (discrete GPU, driver radv), the GPU behind
  /dev/dri/renderD128". If it says "the first GPU listed" instead, the
  Vulkan and VA-API rows may come from different GPUs.
- Vulkan Video is optional. A row is measured only when the device
  decodes the file's first frame (every stream's, for a sample). When it
  doesn't, or a measured run fails later, the row is missing and a note
  says why, in FFmpeg's words where it gives them: for example "decode
  3840x1920 HEVC 8-bit, Vulkan Video (incl. copy to RAM): not measured:
  cannot decode … on Vulkan: the driver does not support this format
  (stream 3840x1920, level 153; FFmpeg: [hevc] Device does not support
  decoding hevc! …)". "Device does not support decoding …" means the
  driver lacks the codec; "Unable to initialize video session: … not
  supported!" that it lacks the profile or pixel format; "Maximum level"
  and "Width"/"Height: from … to …" lines are the limits FFmpeg checks
  the stream against without saying which one failed. Levels are
  FFmpeg's numbers (HEVC 5.1 is 153). Without a Vulkan GPU at all, every
  Vulkan row is missing with that reason. A missing Vulkan row never
  fails the benchmark, and the VA-API and software rows are measured as
  before.
- `vulkan_*_matches_software` and `samples_vulkan_decode_matches_software`
  in the Tier 2 table say whether those decodes are also bit-exact. A
  row can be fast and still wrong, so read the two together.

## Results

### huginn (Radeon 8060S, Fedora 44)

Report: [`hw-reports/huginn-4cd8a0f744.md`](../hw-reports/huginn-4cd8a0f744.md)
at 4cd8a0f, 2026-10-23. Mesa 26.2.5 (radeonsi from
`mesa-va-drivers-freeworld`, RADV), FFmpeg 8.1.3 (RPM Fusion), rustc
1.99.0. Each number is the median of 3 runs after a warm-up.

Tier 2: 8 passed, 0 failed, 1 skipped. `vulkan_hevc_main_matches_software`
skipped because the Vulkan driver does not support the format. Doctor's
decode checks show RADV on Strix Halo decodes AV1 with Vulkan Video, but
not H.264 or HEVC ("Device does not support decoding hevc!"). VA-API
decodes and encodes all four formats it checks.

| Benchmark | VA-API (median, range) | Software (median, range) |
|---|---:|---:|
| X5 2× 1920² @ 119.88 | 1.88 × (1.80 to 1.90) | 1.09 × (1.08 to 1.09) |
| X5 2× 2880² @ 59.94 | 1.61 × (1.53 to 2.03) **unstable** | 0.97 × (0.97 to 0.98) |
| X5 2× 3840² @ 25.00 | 2.43 × (1.52 to 2.87) **unstable** | 0.80 × (0.80 to 0.80) |
| X5 2× 3840² @ 29.97 | 2.40 × (1.17 to 2.41) **unstable** | 0.68 × (0.67 to 0.68) |
| LRV 1× 1664×832 @ 25.00 | 51.70 × (51.44 to 53.62) | 37.29 × (36.73 to 38.27) |
| LRV 1× 1664×832 @ 29.97 | 60.81 × (54.95 to 60.93) | 43.76 × (43.65 to 45.16) |
| 3840×1920 HEVC 8-bit, synthetic | 300.85 fps (299.32 to 304.14) | 716.82 fps (654.90 to 722.33) |
| 5760×2880 HEVC 8-bit, synthetic | 120.79 fps (120.22 to 121.48) | 330.22 fps (307.73 to 360.40) **unstable** |

What this says:

- Software decode can't play the X5's 5.7K and 8K modes in real time even
  on huginn's 16 cores (32 threads), so hardware decode is required, as
  planned.
- VA-API decodes every X5 mode faster than real time, but at 5.7K and 8K
  the runs vary by up to half the median. The slowest 8K30 run, 1.17×,
  leaves little headroom. VA-API times include copying each frame to
  RAM. Keeping frames on the GPU (DMA-BUF into wgpu) is proved in M1,
  with an 8K30 benchmark done both ways, and decided before M3 (D25); the
  M1 viewer's overlay will show how much of each frame's time the copy
  and upload take.
- On the synthetic clips, which are easy to decode, the CPU beats VA-API
  including its copy, as in the first run.

The first run, at a9128cf on 2026-10-22
([`huginn-a9128cf8df-dirty.md`](../hw-reports/huginn-a9128cf8df-dirty.md)),
timed each benchmark once, with no warm-up, and gave 1.18× for X5 8K30 on
VA-API. Its Vulkan Video skip read "Operation not permitted", which was
how Spherecut's refusal of a software fallback showed before 9300fc0.

### odin (RX 7600, Fedora 44)

Report: [`hw-reports/odin-44df411cb7.md`](../hw-reports/odin-44df411cb7.md)
at 44df411, 2026-10-24 (PR #3). Radeon RX 7600 (Navi 33, discrete, the
only GPU, behind `renderD128`), Mesa 26.2.5 with RPM Fusion's
`mesa-vulkan-drivers-freeworld` and `mesa-va-drivers-freeworld`, FFmpeg
8.1.3 (RPM Fusion), rustc 1.99.0. Each number is the median of 3 runs
after a warm-up.

Tier 2: 12 passed, 0 failed, 0 skipped. VA-API and Vulkan Video both
decode H.264, HEVC Main and Main 10 and every X5 sample bit for bit like
software. Doctor found no problems; with `RADV_PERFTEST=video_decode` its
decode checks were the same, so nothing is gated behind the flag.

| Benchmark | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| X5 2× 1920² @ 119.88 | 0.68 × | 1.02 × | 1.66 × |
| X5 2× 2880² @ 59.94 | 0.63 × | 0.95 × (0.42 to 0.95) **unstable** | 1.61 × |
| X5 2× 3840² @ 25.00 | 0.51 × | 1.29 × | 2.23 × |
| X5 2× 3840² @ 29.97 | 0.42 × | 0.81 × (0.48 to 0.82) **unstable** | 2.05 × |
| LRV 1× 1664×832 @ 25.00 | 26.35 × | 23.50 × | 30.40 × |
| LRV 1× 1664×832 @ 29.97 | 28.60 × | 24.70 × | 34.54 × |
| 3840×1920 HEVC 8-bit, synthetic | 259 fps | 55 fps | 288 fps |
| 5760×2880 HEVC 8-bit, synthetic | 118 fps | 47 fps | 135 fps |

Both hardware columns include copying each frame to RAM.

What this says:

- On odin, only Vulkan Video decodes every X5 mode faster than real
  time, with at least 1.6 × to spare and runs within 9 % of each other.
  VA-API falls below real time at 5.7K60 and 8K30, and each of those
  rows had one run at under half the median.
- VA-API's copy to RAM is the likely cost, not its decoder: on the
  synthetic clips it is 4 to 5 times slower than software and than
  Vulkan Video, whose download has the GPU copy the frame into host
  memory. VA-API's download maps the decoded surface and reads it with
  the CPU, which on a discrete GPU is probably a slow read across PCIe
  from video memory (not yet measured on its own; the M1 overlay will
  show it). Keeping frames on the GPU removes that copy for
  both paths, so slice 5b's zero-copy proof (D25) measures both: VA-API
  frames imported into wgpu as DMA-BUFs, and Vulkan Video decoding on
  wgpu's own Vulkan device. Which path is the default is decided from
  those numbers on odin and huginn, before M3.
- Software decode is far from real time at 5.7K and 8K on odin's CPU,
  as on huginn.
- Unified memory against a discrete GPU: huginn's 8060S shares system RAM
  with the CPU, odin's RX 7600 has its own VRAM. VA-API with its copy
  does 301 fps on huginn's synthetic 3840×1920 clip and 55 fps on
  odin's, and X5 8K30 at 2.40 × real time on huginn and 0.81 × on odin.
  The copy costs on huginn too (software beats VA-API there on the
  synthetic clips), so zero-copy helps both, and odin the most, where
  decode and render share the card and nothing need cross PCIe. The
  Framework's Intel iGPU is expected to behave like huginn. huginn has
  no Vulkan Video H.264 or HEVC rows until it gets
  `mesa-vulkan-drivers-freeworld` too.

Second run: [`hw-reports/odin-67fa1fda5c.md`](../hw-reports/odin-67fa1fda5c.md)
at 67fa1fd, 2026-10-24 (PR #4), the same machine and software, 4225 s.
Tier 2: 15 passed, 4 failed, of 19. The failures were this side's, and
are fixed after it: both GPU seek tests failed on keyframe seeks in the
two cropped HEVC clips, since FFmpeg 7.1 and later's HEVC decoder keeps
the frames it has finished across a flush, and a decoder on one thread,
as every hardware decoder is, handed one from before a seek out after it
(D29, D2); and the download pool tests failed on their fault limit and
their own check of the fault counter (D30, m1-plan amendment 12). Exact
seeks, colour, decode and every sample matched software.

| Benchmark | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| X5 2× 1920² @ 119.88 | 0.70 × | 1.03 × | 1.66 × |
| X5 2× 2880² @ 59.94 | 0.63 × | 0.95 × | 1.61 × |
| X5 2× 3840² @ 25.00 | 0.51 × | 1.20 × | 2.23 × |
| X5 2× 3840² @ 29.97 | 0.42 × | 0.83 × (0.82 to 0.91) **unstable** | 1.93 × |
| 3840×1920 HEVC 8-bit, synthetic | 210 fps (183 to 219) **unstable** | 119 fps | 272 fps |
| 5760×2880 HEVC 8-bit, synthetic | 90 fps | 45 fps (42 to 47) **unstable** | 136 fps |

Seek latency, median and p95 in ms (`bench_seek_latency`):

| Seek | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| exact, noisy 5760×2880 HEVC | 678 / 892 | 126 / 245 | 119 / 244 |
| keyframe, noisy 5760×2880 HEVC | 613 / 722 | 50 / 57 | 46 / 51 |
| exact, X5 3840² stream (8K30) | 3719 / 6845 | 182 / 372 | 185 / 382 |
| keyframe, X5 3840² stream | 1659 / 1708 | 22 / 36 | 17 / 28 |
| exact, LRV 1664×832 | 44 / 53 | 18 / 23 | 17 / 21 |
| keyframe, LRV 1664×832 | 27 / 32 | 4.4 / 5.6 | 3.7 / 5.7 |

What this adds:

- The decode rows repeat the first run's within a few per cent, so
  Vulkan Video's lead on odin holds. VA-API's synthetic 3840×1920 row
  doubled (55 to 119 fps), with slice 2b's pooled download in between;
  the 5.7K row didn't move (47 to 45). Software's synthetic rows came out
  lower than the first run's (259 to 210, 118 to 90 fps), not yet
  explained.
- Seeking meets the target on proxies by far: an exact seek in the LRV
  shows its frame in 21 to 53 ms at p95 on every path, against 150 ms.
  On full-resolution X5 footage only the GPU paths are usable (about
  0.2 s for an exact seek, 20 ms for a keyframe one); software takes
  3.7 s, as it decodes a whole GOP of 8K HEVC to reach the frame.
- VA-API's download faults about 10 times per MB copied on odin (116 per
  3840×1920 NV12 frame, 227 per P010 frame), the driver's (D30).

Third and fourth runs:
[`hw-reports/odin-be38fa409e.md`](../hw-reports/odin-be38fa409e.md) at
be38fa4, 2026-10-24 (PR #5, 4230 s), and
[`hw-reports/odin-918d03171c.md`](../hw-reports/odin-918d03171c.md) at
918d031, 2026-10-28 (PR #6, 4124 s), the same machine and software.
Tier 2: 21 of 23 passed at be38fa4, with both golden tests failing
(fixed after it, D31, D32), and all 25 at 918d031. The third run is the
first with `bench_upload_5k7`. Each cell is the third run's, then the
fourth's:

| Benchmark | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| X5 2× 1920² @ 119.88 | 0.68 / 0.72 × | 0.99 / 1.08 × | 1.66 / 1.65 × |
| X5 2× 2880² @ 59.94 | 0.62 / 0.65 × | 0.96 / 1.00 × | 1.61 / 1.64 × |
| X5 2× 3840² @ 25.00 | 0.50 / 0.52 × | 1.19 / 1.35 × | 2.21 / 2.23 × |
| X5 2× 3840² @ 29.97 | 0.42 / 0.44 × | 0.73 / 0.95 × | 1.93 / 1.92 × |
| LRV 1× 1664×832 @ 25.00 | 26.36 / 27.29 × | 23.29 / 25.22 × | 30.22 / 31.87 × |
| LRV 1× 1664×832 @ 29.97 | 29.49 / 30.28 × | 23.51 / 26.31 × | 34.37 / 34.60 × |
| 3840×1920 HEVC 8-bit, synthetic | 206 / 228 fps **unstable** | 118 / 123 fps | 285 / 291 fps |
| 5760×2880 HEVC 8-bit, synthetic | 89 / 98 fps **unstable** | 41 / 45 fps | 131 / 138 fps |

Seek latency at 918d031, median and p95 in ms; the third run's are
within 8% of these:

| Seek | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| exact, noisy 5760×2880 HEVC | 634 / 839 | 129 / 244 | 122 / 242 |
| keyframe, noisy 5760×2880 HEVC | 587 / 712 | 50 / 56 | 46 / 51 |
| exact, X5 3840² stream (8K30) | 3657 / 6618 | 182 / 370 | 183 / 379 |
| keyframe, X5 3840² stream | 1621 / 1676 | 22 / 36 | 17 / 29 |
| exact, LRV 1664×832 | 43 / 51 | 17 / 22 | 17 / 21 |
| keyframe, LRV 1664×832 | 27 / 30 | 4.4 / 5.1 | 3.6 / 4.5 |

Upload (`bench_upload_5k7`), CPU time a step in ms, waited for and two
in flight, and the GPU's ingest and mips (`write_texture`'s; a pair's
per frame), the third run's, then the fourth's:

| Input | `write_texture` | belt | `write_texture`, 2 in flight | belt, 2 in flight | GPU ingest and mips |
|---|---:|---:|---:|---:|---:|
| 5760×2880 NV12 | 4.12 / 2.58 | 17.39 / 16.26 | 4.80 / 2.63 | 3.89 / 3.68 | 0.68 / 1.01 |
| 5760×2880 P010 | 4.80 / 4.34 | 4.78 / 4.36 | 6.54 / 5.82 | 7.58 / 7.71 | 1.24 / 1.24 |
| 3840² NV12 pair | 6.42 / 4.61 | 6.69 / 4.67 | 7.45 / 4.67 | 7.56 / 6.42 | 0.59 / 0.89 |
| 3840² P010 pair | 10.38 / 9.37 | 10.37 / 11.94 | 11.94 / 10.46 | 12.66 / 11.42 | 1.09 / 1.09 |

On the 5.7K slots at 918d031, in GPU ms: the mips alone 1.01 (NV12;
0.62 at be38fa4) and 0.51 (P010), and the projection 0.28 (NV12) and
0.22 to 0.25 (P010) at 1920×1080, rectilinear 90° or a 300° tiny planet,
and 0.74 to 0.75 at 3840×2160.

What these add:

- Vulkan Video's lead holds. VA-API's X5 rows rose 4 to 30% at 918d031;
  its 8K30 row has come out between 0.73 and 0.95 × over the four runs,
  under real time each time.
- Uploading and drawing a frame is cheap next to its 33 ms at 30 fps:
  at 918d031 `write_texture` takes 2.58 ms of CPU for a 5.7K NV12 frame
  and 4.34 ms for P010, the GPU's ingest and mips 1.01 and 1.24 ms, and
  the projection under 0.3 ms at 1080p and 0.75 ms at 4K.
- The 8-bit mips cost 0.39 ms more since the mip pass works its means
  out in integers (D31); the deep planes' f16 path didn't change.
- `write_texture` got 28 to 37% cheaper at 8 bits and about 10% at 10
  bits between the runs, with no change to the upload, and the belt's
  3840² pair as much: not yet explained.
- The belt's waited 5.7K NV12 step makes or frees allocator blocks every
  time and costs 16 to 17 ms; two in flight it costs 3.7 to 3.9 ms.
  `write_texture` never makes or frees one within a step. The CPU
  trigger has held only through its 4 ms arm (both 5.7K inputs at
  be38fa4, P010 alone at 918d031), where the belt was no cheaper, so
  the default stays `write_texture` until H1 (D31).
- VA-API's download still faults about 10 times per MB copied (10.2 to
  10.5); Vulkan Video's none.

Fifth run, H1's first:
[`hw-reports/odin-43abcdad28.md`](../hw-reports/odin-43abcdad28.md) at
43abcda, 2026-10-29 (PR #7, 8936 s, of which 3838 s made the perf
clips), the same machine and software, with kernel 7.2.9 and the
CPU's governor and energy preference at `performance`. Tier 2: 28 of
31 passed, both 5.7K gates failing, and `bench_playback_x5_raw` passed
on passes that had stalled at their second frame (both fixed after it;
PROGRESS, "odin's fifth run"). Decode and seek, against the fourth
run's:

| Benchmark | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| X5 2× 1920² @ 119.88 | 0.72 × | 1.08 × | 1.66 × |
| X5 2× 2880² @ 59.94 | 0.66 × | 1.02 × | 1.62 × |
| X5 2× 3840² @ 25.00 | 0.53 × | 1.38 × | 2.20 × |
| X5 2× 3840² @ 29.97 | 0.45 × | 1.04 × | 1.93 × |
| LRV 1× 1664×832 @ 25.00 | 27.20 × | 23.79 × | 31.17 × |
| LRV 1× 1664×832 @ 29.97 | 31.26 × | 24.35 × | 34.90 × |
| 3840×1920 HEVC 8-bit, synthetic | 230 fps (188 to 250) **unstable** | 129 fps | 285 fps |
| 5760×2880 HEVC 8-bit, synthetic | 101 fps | 48 fps | 135 fps |

| Seek (median / p95, ms) | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| exact, noisy 5760×2880 HEVC | 638 / 855 | 131 / 245 | 123 / 244 |
| keyframe, noisy 5760×2880 HEVC | 586 / 698 | 50 / 57 | 45 / 51 |
| exact, X5 3840² stream (8K30) | 3641 / 6699 | 182 / 370 | 184 / 380 |
| keyframe, X5 3840² stream | 1623 / 1685 | 22 / 36 | 17 / 28 |
| exact, LRV 1664×832 | 43 / 50 | 18 / 23 | 17 / 21 |
| keyframe, LRV 1664×832 | 26 / 29 | 4.3 / 5.3 | 3.6 / 4.4 |

Upload (`bench_upload_5k7`), as above:

| Input | `write_texture` | belt | `write_texture`, 2 in flight | belt, 2 in flight | GPU ingest and mips |
|---|---:|---:|---:|---:|---:|
| 5760×2880 NV12 | 2.57 | 15.84 | 2.63 | 3.74 | 1.01 |
| 5760×2880 P010 | 4.31 | 4.32 | 5.93 | 6.21 | 1.24 |
| 3840² NV12 pair | 5.83 | 5.86 | 6.64 | 6.63 | 0.88 |
| 3840² P010 pair | 9.26 | 9.31 | 10.60 | 10.82 | 1.09 |

The playback benchmarks' first numbers, on the perf clips (D37), each
pass's decoded fps per second, unthrottled and in real time, then what
dropped after the first second and the stages at p50 / p95 in ms. The
5.7K rows are from odin's reruns at 1991a98 over Remote Control, after
the fixes (10-bit Vulkan Video's from the third, the rest from the
second); the 8K rows are the fifth run's, at 43abcda:

| Clip, decoder | Unthrottled | Real time | Dropped, decode-dropped | `download` | `upload_cpu` |
|---|---:|---:|---:|---:|---:|
| 5.7K 8-bit, Vulkan (Auto) | 121 (120 to 130) | 30 (0 to 129) | 0, 0 | 2.57 / 10.98 | 2.99 / 3.47 |
| 5.7K 8-bit, VA-API | 28 (25 to 51) | 28.5 (25 to 54) | 33, 33 | 31.16 / 66.02 | 5.42 / 6.37 |
| 5.7K 8-bit, software | 46.5 (40 to 53) | 30 (0 to 51) | 0, 0 | | 4.38 / 6.15 |
| 5.7K 10-bit, Vulkan (Auto) | 88 (62 to 98) | 30 (0 to 113) | 0, 0 | 4.97 / 25.82 | 5.89 / 7.22 |
| 5.7K 10-bit, VA-API | 17 (15 to 18) | 24 (22 to 28) | 409, 411 | 109.38 / 150.42 | 6.09 / 7.28 |
| 5.7K 10-bit, software | 33 (28 to 38) | 30 (0 to 40) | 20, 12 | | 11.88 / 16.14 |
| 8K 8-bit, Vulkan (Auto) | 38 (19 to 71) | 29 (0 to 61) | 34, 34 | 22.27 / 47.27 | 4.96 / 7.19 |
| 8K 8-bit, VA-API | 12 (10 to 15) | 24 (18 to 30) | 445, 454 | 123.79 / 221.99 | 5.98 / 6.80 |
| 8K 8-bit, software | 30 (24 to 31) | 30 (19 to 36) | 216, 183 | | 10.10 / 14.05 |

What these add:

- On odin only Vulkan Video plays 5.7K in real time. VA-API's download,
  the CPU reading the surface across PCIe, takes 31 ms a 5.7K NV12
  frame and 109 ms a P010 one, and it dropped 33 of 8-bit's frames and
  409 of 10-bit's in 20 s; software kept up at 8 bits and nearly at 10.
  Slice 5b's zero-copy removes both downloads.
- The 8K row predates the fixes: its downloads went into fresh 4 KiB
  pages while the cache filled, as the 5.7K ones did then (`download`
  22 ms at p50), so it is to be measured again.
- `download` and `upload_cpu` run on two threads, and even at 10 bits
  their p50s add up to 10.9 ms, under the third of a frame (11.1 ms)
  that would call for U2 (m1-plan section 4.4, D31).
- 10-bit's unthrottled pass ran at 17 to 20 fps for 3 s in one run at
  1991a98, while the cache passed 5 GiB, and at 48 fps or more in the
  next three, one with free memory fragmented (PROGRESS, "odin's fifth
  run").

Sixth run, H1 at main:
[`hw-reports/odin-303e3644be.md`](../hw-reports/odin-303e3644be.md) at
303e364, 2026-10-30 (PR #8, 9032 s, of which 3875 s made the perf
clips), the same machine and software. Tier 2: 31 of 32 passed and
`bench_playback_stitched_samples` skipped (no clip 12); Tier 1 failed
one engine test that needed more time under load (PROGRESS, "odin's
sixth run"). Decode and seek, against the fifth run's:

| Benchmark | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| X5 2× 1920² @ 119.88 | 0.72 × | 1.07 × | 1.66 × |
| X5 2× 2880² @ 59.94 | 0.65 × | 0.99 × | 1.61 × |
| X5 2× 3840² @ 25.00 | 0.52 × | 0.60 × | 2.23 × |
| X5 2× 3840² @ 29.97 | 0.44 × | 0.43 × | 1.94 × |
| LRV 1× 1664×832 @ 25.00 | 27.83 × | 24.08 × | 29.84 × |
| LRV 1× 1664×832 @ 29.97 | 31.88 × | 25.45 × | 34.55 × |
| 3840×1920 HEVC 8-bit, synthetic | 219 fps (180 to 229) **unstable** | 124 fps (57 to 125) **unstable** | 287 fps |
| 5760×2880 HEVC 8-bit, synthetic | 95 fps | 43 fps | 134 fps |

| Seek (median / p95, ms) | Software | VA-API | Vulkan Video |
|---|---:|---:|---:|
| exact, noisy 5760×2880 HEVC | 642 / 846 | 126 / 245 | 119 / 244 |
| keyframe, noisy 5760×2880 HEVC | 592 / 694 | 55 / 62 | 45 / 51 |
| exact, X5 3840² stream (8K30) | 3623 / 6653 | 182 / 370 | 183 / 380 |
| keyframe, X5 3840² stream | 1614 / 1656 | 22 / 35 | 17 / 28 |
| exact, LRV 1664×832 | 42 / 50 | 18 / 22 | 17 / 21 |
| keyframe, LRV 1664×832 | 26 / 29 | 4.3 / 5.0 | 3.7 / 5.0 |

Upload (`bench_upload_5k7`): within 4 % of the fifth run's at every
input (5.7K NV12 2.63 ms with `write_texture`, P010 4.47, the 3840²
pairs 5.85 and 9.44; GPU ingest and mips 1.01, 1.24, 0.89 and 1.09).

Playback, after H1's fixes, as in the table above:

| Clip, decoder | Unthrottled | Real time | Dropped, decode-dropped | `download` | `upload_cpu` |
|---|---:|---:|---:|---:|---:|
| 5.7K 8-bit, Vulkan (Auto) | 121 (119 to 130) | 30 (0 to 129) | 0, 0 | 2.59 / 11.30 | 3.69 / 5.04 |
| 5.7K 8-bit, VA-API | 27 (25 to 49) | 28 (25 to 49) | 30, 30 | 31.20 / 55.27 | 5.58 / 6.47 |
| 5.7K 8-bit, software | 47.5 (40 to 56) | 30 (0 to 54) | 0, 0 | | 4.35 / 6.39 |
| 5.7K 10-bit, Vulkan (Auto) | 90 (60 to 98) | 30 (0 to 102) | 0, 0 | 5.08 / 23.98 | 6.37 / 7.71 |
| 5.7K 10-bit, VA-API | 17 (16 to 18) | 25 (23 to 28) | 408, 408 | 109.92 / 146.69 | 6.07 / 6.94 |
| 5.7K 10-bit, software | 35 (26 to 38) | 30 (0 to 40) | 5, 0 | | 12.03 / 15.84 |
| 8K 8-bit, Vulkan (Auto) | 81 (67 to 84) | 30 (0 to 61) | 0, 0 | 4.59 / 27.06 | 6.10 / 7.04 |
| 8K 8-bit, VA-API | 14 (12 to 18) | 24 (21 to 31) | 434, 434 | 121.90 / 187.45 | 5.81 / 6.50 |
| 8K 8-bit, software | 30 (24 to 33) | 30 (13 to 36) | 158, 125 | | 10.35 / 14.42 |
| X5 8K30, Vulkan (Auto) | 79 (76 to 81) | 30 (29.5 to 60) | 0, 0 | 3.83 / 14.70 | 3.78 / 5.65 |
| X5 8K30, VA-API | 24 (22 to 28.5) | 30 (27 to 31.5) | 249, 244 | 35.07 / 69.57 | 4.96 / 6.08 |
| X5 8K25, Vulkan (Auto) | 75 | 25 (0 to 51) | 0, 0 | 9.96 / 29.20 | 2.55 / 3.50 |
| X5 8K25, VA-API | 42 (42 to 42.5) | 25 (0 to 44.5) | 0, 0 | 21.90 / 40.56 | 2.51 / 3.73 |

The X5 rows decode two lenses, each with its own decoder; their fps is
per lens. What these add:

- The 8K row measured again after the huge pages: Vulkan Video's
  download fell from 22 to 4.6 ms at p50, and 8K plays with nothing
  dropped. Every Vulkan Video run plays in real time, X5 8K30 included,
  with 2.6 × to spare unthrottled.
- VA-API's download is still the wall (31, 110 and 122 ms at p50), and
  zero copy (slice 5b, below) is what lifts it.
- VA-API's decode of the X5's 8K modes in `bench_samples_realtime_decode`
  came out at 0.60 and 0.43 × against the fifth run's 1.38 and 1.04,
  every run of the three alike, while the other modes and the
  synthetic decodes moved by 10 % or less. The first run had one run
  of each 8K30 and 2.9K60 at under half the median, so VA-API at these
  sizes has been slow before; slice 5b's odin run runs the bench at
  43abcda and at main to tell code from machine.
- It was the code: 43abcda ran at 1.32 and 0.93 ×, main at 0.61 and
  0.46 ×, and with transparent huge pages off (`PR_SET_THP_DISABLE`)
  main stayed at 0.60 and 0.43 ×. A test that played the 3840² modes
  with each of five allocators for the download pool, five rounds,
  twice (branch `diag/vaapi-8k-alloc`), had `av_buffer_alloc` at 1.32
  to 1.39 × at 25 fps and 0.95 to 1.08 × at 29.97 in all 20 runs, and
  every mapping (2 MiB aligned or not, advised or not, the data at the
  start or 64 bytes in) at about half on most runs. The speed followed
  the minor faults: 93 000 to 208 000 a run when fast, 0.7 to 3.4
  million when slow. 2880² was 1.0 × under every allocator. The faults
  aren't the pool's own: that bench's decoders take two buffers each,
  some 11 000 faults at 3840², and why the driver's side faults more
  beside mappings isn't known. So the pool takes VA-API's buffers from
  `av_buffer_alloc` again, and keeps the huge-page mappings for Vulkan
  Video's, which they made play (m1-plan amendment 18).
- With that fix (d8a9054) `bench_samples_realtime_decode` ran twice:
  VA-API at 1.39 and 1.39 × on the X5's 8K25 and 1.04 and 1.02 × on
  8K30, every run of three within 0.04 of its median; the 1920² and
  2880² modes, software and Vulkan Video within 4 % of before, and the
  LRV proxies within 16 % either way, as their small, fast decodes have
  moved before. Both 5.7K
  gates passed, Vulkan Video's rows as in the table above (unthrottled
  122.5 and 87 fps, nothing dropped) and VA-API's as before (download
  36 and 148 ms at p50): the noisy 5.7K clips weren't slowed by the
  mappings, and the copy is still VA-API's wall there. The download
  pool tests passed; VA-API's 3840×1920 NV12 took 1787 minor faults a
  frame in two runs of three where earlier runs took 116, the same two
  states as the 8K decode's, with the pool allocating nothing after
  the warm-up. The faults are the driver's (amendment 12).

Zero copy, path A (slice 5b, at f3e860d, PR #9,
`hw-reports/odin-5b-f3e860d/`): `zero_copy_matches_copy_path` passed
(HEVC Main, Main 10 and an X5 lens, 48 frames each, every import
byte-identical to the copy), `zero_copy_frame_matches_the_copy` passed
(H.264, HEVC Main and Main 10), and an 8K perf clip frame rendered
through `frame --zero-copy` was the same PNG as through `--decoder
vaapi`. `bench_x5_8k30_zero_copy_vs_copy`, unthrottled fps per lens and
the stages' p50 in ms, VA-API copied / VA-API imported / Vulkan Video
copied:

| X5 mode | Unthrottled | `upload_cpu` | `download` | Dropped after 1 s |
|---|---:|---:|---:|---:|
| 2× 1920² @ 119.88 | 142 / 265 / 259 | 1.03 / 0.32 / 1.00 | 6.24 / 1.70 / 2.94 | at the 60 Hz display's rate on all three, none decode-dropped |
| 2× 2880² @ 59.94 | 66 / 136 / 135 | 1.67 / 0.31 / 1.67 | 13.94 / 0.02 / 5.93 | 0 / 0 / 0 |
| 2× 3840² @ 25 | 24 / 76 / 63 | 2.55 / 0.50 / 3.61 | 22.46 / 0.03 / 13.00 | 0 / 25 (23 decode-dropped) / 0 |
| 2× 3840² @ 29.97 | 41 / 59 / 78 | 4.27 / 0.95 / 3.82 | 26.03 / 0.02 / 3.76 | 0 / 0 / 0 |

No import was refused (1175 to 3579 a mode). Imported, VA-API decodes
the 1920² and 2880² modes about twice as fast as copied, level with
Vulkan Video copied, and its upload's CPU time falls to a third or
less; the "download" left is the map. At 3840² it is three times as
fast at 25 fps and half again at 29.97, where Vulkan Video copied
stays ahead. VA-API's copied 3840² @ 25 row, 24 fps, is under the
sixth run's 42, as the slow allocator above would make it.

Zero copy, path B (slice 5b, at a13ee07, PR #10,
`hw-reports/odin-5b-b-a13ee07/`): `shared_vulkan_matches_copy_path`
passed (the same clips on Vulkan Video, 48 frames each, every copy out
of FFmpeg's image byte-identical to the download),
`shared_vulkan_frame_matches_the_copy` passed, and path A's check again.
The bench, VA-API copied / VA-API imported / Vulkan Video copied /
Vulkan Video copied out on the shared device (still on the slow
allocator, so VA-API's copied 8K rows are low):

| X5 mode | Unthrottled | `upload_cpu` p50 | `upload_gpu` p50 | `download` p50 | Dropped after 1 s (decode-dropped) |
|---|---:|---:|---:|---:|---:|
| 2× 1920² @ 119.88 | 136 / 256 / 255 / 277 | 0.82 / 0.27 / 0.83 / 0.56 | 0.28 / 0.40 / 0.28 / 0.35 | 6.16 / 1.89 / 3.00 / 0 | at the 60 Hz display's rate, none decode-dropped |
| 2× 2880² @ 59.94 | 68 / 136 / 133 / 143 | 1.51 / 0.29 / 1.52 / 0.76 | 0.55 / 0.82 / 0.56 / 0.74 | 13.77 / 0.01 / 6.08 / 0 | 0 on all four |
| 2× 3840² @ 25 | 43 / 76 / 74 / 78 | 2.49 / 0.33 / 2.50 / 1.46 | 1.70 / 2.07 / 1.71 / 2.06 | 21.84 / 0.02 / 9.94 / 0 | 0 / 19 (19) / 0 / 2 (2) |
| 2× 3840² @ 29.97 | 20 / 78 / 78 / 81 | 3.40 / 0.37 / 3.92 / 1.07 | 0.91 / 1.55 / 0.93 / 1.35 | 24.55 / 0.02 / 3.80 / 0 | 0 on all four |

No frame was refused on either path (1183 to 4440 a mode). Path B is
the fastest unthrottled in every mode, 3 to 9 % ahead of Vulkan Video
copied and VA-API imported, with a quarter to two thirds of Vulkan
Video copied's upload CPU time (its wait for the copy is in
`upload_cpu`) and no download at all. 3840² @ 25 is the one mode where zero copy decode-dropped in real
time, 19 frames imported and 2 copied out, as path A's 25 before.
Seventh run, H2:
[`hw-reports/odin-c9a4a55c18.md`](../hw-reports/odin-c9a4a55c18.md) at
c9a4a55, 2026-10-30 (PR #11, 6391 s), the same machine and software.
Tier 2: 37 of 39 passed, `gui_smoke` lost every run to a locked screen
(the rerun is below) and `bench_playback_stitched_samples` skipped (no
clip 12). Decode, seek and upload were within 10 % of the sixth run's,
except VA-API on the X5's 3840² streams: its decode back at 1.40 and
1.04 × real time with the download pool's allocator fix (d8a9054), and
its keyframe seek at 33 ms against 22. Playback, as in the sixth run's
table:

| Clip, decoder | Unthrottled | Real time | Dropped, decode-dropped | `download` | `upload_cpu` |
|---|---:|---:|---:|---:|---:|
| 5.7K 8-bit, Vulkan (Auto) | 123 (121 to 131) | 29.5 (0 to 129) | 0, 0 | 2.56 / 11.32 | 2.89 / 3.33 |
| 5.7K 8-bit, VA-API | 22 (21 to 30) | 28 (24 to 39) | 135, 135 | 33.06 / 93.60 | 3.55 / 3.96 |
| 5.7K 8-bit, software | 50 (44 to 57) | 30 (0 to 54) | 0, 0 | | 4.38 / 6.02 |
| 5.7K 10-bit, Vulkan (Auto) | 85 (64 to 109) | 30 (0 to 111) | 0, 0 | 5.16 / 24.09 | 6.09 / 6.87 |
| 5.7K 10-bit, VA-API | 14 (13 to 18) | 24 (21 to 26) | 448, 450 | 144.47 / 190.30 | 5.95 / 6.48 |
| 5.7K 10-bit, software | 37 (31 to 41) | 30 (0 to 41) | 0, 0 | | 10.89 / 14.98 |
| 8K 8-bit, Vulkan (Auto) | 81 (73 to 85) | 30 (0 to 61) | 0, 0 | 4.64 / 27.11 | 5.25 / 6.18 |
| 8K 8-bit, VA-API | 12.5 (10 to 15) | 24.5 (17 to 31) | 452, 455 | 115.86 / 215.29 | 5.70 / 6.03 |
| 8K 8-bit, software | 30 (26 to 37) | 30 (0 to 37) | 72, 56 | | 9.40 / 13.28 |
| X5 8K30, Vulkan (Auto) | 77 (75 to 80.5) | 30 (29.5 to 60) | 0, 0 | 4.86 / 11.51 | 3.99 / 5.05 |
| X5 8K30, VA-API | 23 (18 to 39.5) | 30 (25.5 to 42) | 0, 0 | 26.24 / 47.38 | 4.73 / 6.31 |
| X5 8K25, Vulkan (Auto) | 75 | 0 (0 to 50.5) | 0, 0 | 9.59 / 28.96 | 2.43 / 3.68 |
| X5 8K25, VA-API | 43.75 (43 to 44.5) | 0 (0 to 44.5) | 0, 0 | 22.57 / 43.11 | 2.46 / 3.52 |

What these add:

- Every Vulkan Video row plays in real time with nothing dropped, as
  in the sixth run, and the headless benches are clean where the
  window, below, wasn't.
- Vulkan Video's `upload_cpu` at 8 bits is a fifth lower than the
  sixth run's (2.89 against 3.69 ms at 5.7K, 5.25 against 6.10 at 8K),
  not yet explained.
- VA-API copied is still the slow path, and noisy: its X5 8K30 decode
  ran at 23 fps a lens here and 43 in the zero-copy bench's run of the
  same mode. The zero-copy bench's imported, Vulkan Video and path B
  rows were within 2 % of PR #10's, path B again the fastest
  unthrottled in every mode, and both zero-copy paths again dropped at
  3840² @ 25 in real time (29 frames imported and 26 on path B, all
  but one of each decode-dropped).

The GUI, `gui_smoke` (D40), rerun with the screen unlocked at 45267d1
(PR #11, [`hw-reports/odin-45267d1-gui.md`](../hw-reports/odin-45267d1-gui.md)):
the seam clip and the sync flash and beep passed at 100 % presented
and a UI p99 of 17.0 ms, and all five screenshots were right. The
noisy 5.7K 8-bit clip dropped 140 of 599 frames with a UI p99 of
49.9 ms, and the 10-bit clip and the X5 pair presented 69 % and 75 %
of the UI frames expected, the UI at about 30 Hz for stretches, which
the gate then read as the compositor not drawing (fixed: D40). In the
window, Vulkan Video's `download` was 19.2 / 73.4 ms at 5.7K 8-bit,
against 2.56 / 11.32 headless. Smoke runs of the three on their own
(branch `diag/gui-5k7`, 20 s each), the presented share of the UI
frames expected and eframe's own work per UI pass at p50:

| Clip | Copy, `write_texture` | Copy, staging belt | Zero copy, path B |
|---|---:|---:|---:|
| 5.7K 8-bit | 84 % and 77 %, 3.0 ms | 86 %, 1.3 ms | 96 %, 0.9 ms |
| 5.7K 10-bit | 51 %, 6.4 ms | 81 %, 1.5 ms | 94 %, 0.9 ms |
| X5 8K30 pair | 64 %, 4.6 ms | 98 %, 1.3 ms | 99 %, 1.05 ms |

`write_texture` holds wgpu's queue locked through its copy of each
frame, and the UI thread's own buffer writes and submit wait for it;
the belt doesn't, and is now the player's upload path (D31). What the
belt leaves is the copy path's own traffic, the download and the
upload of every frame on the one queue the window presents from. Zero
copy has neither, and missed 3 to 5 frames in 20 s (UI p99 22 to
50 ms); it is now the default for Vulkan Video (D25). The GPU was 16 % busy at 1065 MHz shader and 456 MHz memory
clocks under amdgpu's automatic power levels, the CPU 94 % idle, and a
browser was playing video in the background. The headless
`bench_playback_realtime_5k7_8bit` passed meanwhile.

With zero copy the default, `gui_smoke` at 36041f7 (PR #11) presented
95 %, 94 % and 99 % on the noisy 5.7K 8- and 10-bit clips and the X5
pair, with 39, 44 and 16 late UI passes after the first second, the
UI's own work under 1.5 ms and its acquire under 0.2 ms. Each late gap
came after the present: RADV lists the swapchain image the UI holds in
every submission on its device, amdgpu makes each a writer of it, and
mutter waits for them, an I-frame decode's 40 ms included (D41).
Diagnostic smoke runs (branch `diag/late-ui`, 20 s each): the late UI
passes after the first second in each run, and presented and the UI's
p99 in the first:

| Run | Late | Presented | UI p99 |
|---|---:|---:|---:|
| 5.7K 8-bit | 27, 37, 28 | 95.8 % | 49.9 ms |
| 5.7K 8-bit, the gate | 1, 2 | 99.9 % | 17.05 ms |
| 5.7K 8-bit, the gate held 5 ms | 2 | | |
| 5.7K 8-bit, no mips | 16 | 98 % | 33 ms |
| 5.7K 10-bit | 54 | 91.7 % | |
| 5.7K 10-bit, the gate | 2 | 99.8 % | 17.08 ms |
| X5 8K30 pair | 5 | 99.55 % | |
| X5 8K30 pair, the gate | 3 | 99.7 % | |

The gate's longest wait was 2.1 ms (5.1 ms held 5 ms), and no run with
it dropped a frame. Of the late passes without it, 20 of 27, 32 of 37,
25 of 28, 53 of 54 and 5 of 5 had a decode or upload submitted between
the acquire and the present; the rest an I-frame decode still running.
The few left with it are 26 to 33 ms gaps with nothing submitted: the
compositor's own.

`gui_smoke` with the gate (branch `m1/submit-gate` at 0a72983, an
early check before main) passed all five runs: presented 99.8 % to
99.9 %, the UI's p50 / p99 16.7 / 17.0 to 17.1 ms, nothing dropped,
one late pass a run of 28 to 30 ms with nothing slow in its gap, and
the UI's own work 0.9 / 1.1 ms (1.0 / 1.4 on the X5 pair). The gate
held 813, 829, 230, 14 and 1023 submissions (the 5.7K clips, the seam,
the sync clip, the X5 pair), the longest 2.3 ms. The `decode` and
`upload_cpu` stages now count those waits: their p50 went from about
0.7 to 2.8 ms and from 1.2 to 3.1 ms at 5.7K.


Eighth run, H3:
[`hw-reports/odin-0d491ae.md`](../hw-reports/odin-0d491ae.md) at
0d491ae, 2026-10-30 (PR #12, 5868 s), the same machine and software,
with the submit gate (D41). Tier 1 on odin: 767 of 767. Tier 2: 36 of
40 passed, the stitched-samples bench skipped (no clip 12), and three
failed. `gui_smoke` passed all five runs at 100 % presented, the UI's
p99 at 17.0 to 17.2 ms and nothing dropped, the gate's longest wait
2.48 ms.

- Both 5.7K gates failed on Vulkan Video's copy path, the gated run
  then: the 10-bit clip's unthrottled seconds fell to 11 to 15 fps for
  five seconds (61, 43, 14, 11, 15, 14, 11, 31, then 95 to 99) and its
  real-time pass dropped 69 frames between seconds 5 and 9; the 8-bit
  clip's fell to 27 (109, 110, 66, 32, 27, 87, 130). The 10-bit rerun,
  with 15.7 GB free, passed; an 8-bit rerun with 4.5 GB free fell to
  21. The machine had about 46 GB of page cache: each new huge-page
  download buffer compacted memory directly (THP defrag `madvise`;
  `compact_stall` up about 15 000 and `thp_fault_fallback` 12 500 over
  the failing run) while the cache filled. The gated run is now the
  app's own path, zero copy (D42).
- `zero_copy_matches_copy_path` (path A, opt-in) failed once: an X5
  lens's frame 26 imported rendered other bytes than its download. Its
  rerun passed, and so did ten more with the diagnostics that would say
  how (below); the cause isn't known.

At 8d5c943 (branch `m1/h3`, over Remote Control), with about 15.5 GB
free and no compaction, both gates passed twice, on path B. Each gated
run's 60 s, and `Auto`'s copy path recorded for 20 s, run 1 / run 2:

| Clip, path | Unthrottled | Real time, decoded | Dropped, decode-dropped, late | `decode` p50 / p95 | `upload_cpu` p50 / p95 | `upload_gpu` p50 / p95 |
|---|---:|---:|---:|---:|---:|---:|
| 5.7K 8-bit, zero copy | 129.5 (128 to 131) / 129 (129 to 131) | 30 (12 to 46) | 0, 0, 0 | 0.66 / 23.82 | 1.29 / 1.49 | 1.77 / 1.85 |
| 5.7K 8-bit, copied | 110 (108 to 128) / 109 (108 to 129) | 30 (0 to 61) | 0, 0, 0 | 1.17 / 21.44 | 3.60 / 3.93 | 1.04 / 2.07 |
| 5.7K 10-bit, zero copy | 129 (129 to 130) / 129.5 (128 to 130) | 30 (12 to 46) | 0, 0, 0 | 0.54 / 31.23 | 1.63 / 1.91 | 2.52 / 2.87 |
| 5.7K 10-bit, copied | 84 (63 to 107) / 88.5 (64 to 109) | 29.5 (0 to 61) | 0, 0, 0 | 1.15 / 20.69 | 6.43 / 7.26 | 1.49 / 4.08 |

The stages are run 1's; run 2's were within 0.2 ms. Path B took 2385
to 2388 frames on the GPU a run and refused none, and downloaded
nothing; the copied rows' `download` was 5.05 / 28.76 ms (10-bit) and
6.57 / 27.18 ms (8-bit). Zero copy decodes the 10-bit clip half again
as fast as the copy, and the 8-bit clip a sixth faster, unthrottled;
its real-time decode keeps no more than the decoder's surfaces ahead
(12 to 46 a second), where the copy fills the cache at up to 61. These
runs had memory to spare, so they don't show the copy path's slump.
`zero_copy_matches_copy_path` passed 10 of 10 meanwhile.

Ninth run, M3.1's pair decode:
[`hw-reports/odin-0444568-k1k3.log`](../hw-reports/odin-0444568-k1k3.log)
at 0444568 (branch `m3/decode`, over Remote Control), 2026-11-01 (PR
#27). `bench_export_pair_decode` on clip 9 (the X5's 8K30, 2×3840²),
600 frames a lens, in 1131 s; MemFree read 0 GB before it, with about
44 GB of page cache. K1, beside a VA-API H.264 encode of a 3840×2160
frame at 30 fps, the slower lens's fps (runs; median):

| Path | Serial | A thread a lens |
|---|---:|---:|
| Vulkan Video download | 33.2, 33.2, 33.1; 33.2 | 32.0, 32.0, 31.9; 32.0 |
| VA-API download | 18.5, 18.4, 18.5; 18.5 | 14.2, 14.3, 14.1; 14.2 |
| Software | 13.6 | 13.8 |

K1 is Redirect 2 (M3 plan 4.3): no setup reaches 36 fps a lens. K3, the
same clip alone, each run in a child process of its own, peak RSS
growth in MiB:

| Path | Lens 0 alone | Pair, serial | Pair, a thread a lens | Pass at 2.2 × lens 0 |
|---|---:|---:|---:|---:|
| VA-API download | 246 | 422 | 443 | 542 |
| Vulkan Video download | 225 | 410 | 406 | 496 |
| Software | 1230 | 2451 | 2451 | 2705 |

K3 passes on every path; each pair run had 10 frames alive at its peak
(211 MiB), lens 0 alone 5. In those runs alone, Vulkan Video decoded
one lens at 155 fps and the pair at 78 to 79 a lens; VA-API's lens 0
alone at 28.6 and the serial pair's slower lens at 41.3, with a thread
a lens at 12.9, which isn't explained yet.

### Framework 13 Pro (Intel Xe3, Arch)

Not available yet.

## Reference: software decode in the cloud workspace

For scale only (4 vCPUs, FFmpeg 6.1.1, not a target machine): the X5 8K30
take decodes in software at about 12 frames per second for one 3840²
stream at a time. Playback needs both streams at 29.97 fps, so this is far
from real time.
