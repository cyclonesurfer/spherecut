# Performance

Measured numbers per machine, so regressions show up. Each row comes from a
`cargo xtask hw-test` report (release build); the report's commit is in the
table. Numbers from the GPU-less cloud workspace are not recorded here.

## Targets (from `docs/PROMPT.md`)

| Target | Measured by |
|---|---|
| The UI stays at display refresh even when decode falls behind | GUI smoke run frame times (from M1) |
| 5.7K30 raw or stitched footage plays in real time with hardware decode on odin and huginn | `bench_samples_realtime_decode`, `bench_decode_hevc_4k_and_5k7`; playback with the overlay from M1 |
| 8K30 plays in real time, at least on proxies | the same, on 8K samples and their LRV proxies |
| A random seek shows a frame in under ~150 ms on proxies | seek benchmark (M1) |
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
  3840×1920 and 5760×2880 HEVC 8-bit clip, in software and on VA-API, in
  frames per second. The VA-API number includes copying each frame to
  system memory, which the render path will avoid later.
- **`bench_samples_realtime_decode`**: for each recording mode in
  `samples/`, takes the longest file, decodes up to ten seconds of all its
  video streams at once, one thread per stream as playback does, and
  reports the slowest stream as a multiple of real time (1.0 = just keeps
  up). An Insta360 X5 8K30 file has two 3840² streams, so 1.0 means 60
  decoded 3840² frames per second.

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
  RAM. Keeping frames on the GPU (DMA-BUF into Vulkan) is in `PLAN.md`'s
  M9 if profiling shows it matters; the M1 viewer's overlay will show how
  much of each frame's time the copy and upload take.
- On the synthetic clips, which are easy to decode, the CPU beats VA-API
  including its copy, as in the first run.

The first run, at a9128cf on 2026-10-22
([`huginn-a9128cf8df-dirty.md`](../hw-reports/huginn-a9128cf8df-dirty.md)),
timed each benchmark once, with no warm-up, and gave 1.18× for X5 8K30 on
VA-API. Its Vulkan Video skip read "Operation not permitted", which was
how Spherecut's refusal of a software fallback showed before 9300fc0.

### odin (RX 7600, Fedora)

Not run yet: no report.

### Framework 13 Pro (Intel Xe3, Arch)

Not available yet.

## Reference: software decode in the cloud workspace

For scale only (4 vCPUs, FFmpeg 6.1.1, not a target machine): the X5 8K30
take decodes in software at about 12 frames per second for one 3840²
stream at a time. Playback needs both streams at 29.97 fps, so this is far
from real time.
