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

Report: [`hw-reports/huginn-a9128cf8df-dirty.md`](../hw-reports/huginn-a9128cf8df-dirty.md)
at a9128cf (the `-dirty` is untracked scratch files only, no source
changes), 2026-10-22. Mesa 26.2.5 (radeonsi from
`mesa-va-drivers-freeworld`, RADV), FFmpeg 8.1.3 (RPM Fusion), rustc
1.99.0. The GPU was idle for the whole run: nothing else was loaded on it.

This run predates the repeat-and-spread change above: each number is a
single run, with no warm-up. It also predates the hw-test, doctor,
decoder and probe fixes merged since a9128cf, from both M0 reviews. A
rerun at M0_COMMIT is pending.

Tier 2: 7 passed, 0 failed, 1 skipped. `vulkan_hevc_main_matches_software`
skipped: Vulkan Video HEVC decode unavailable ("decoding: Operation not
permitted"). That message is how error code -1 reads: the decoder passed
on the `AV_PIX_FMT_NONE` (-1) that Spherecut's `get_format` callback
returns when it refuses to fall back to software. The decoder now reports
why instead (this FFmpeg has no hardware decoder for the format, or the
driver refused it), and doctor's Vulkan decode checks show which formats
fail. `spherecut doctor` (the version at a9128cf, which only opened the
devices): no problems found.

| Benchmark | VA-API | Software |
|---|---:|---:|
| X5 2× 1920² @ 119.88 | 1.87 × real time | 1.08 × real time |
| X5 2× 2880² @ 59.94 | 2.00 × real time | 0.96 × real time |
| X5 2× 3840² @ 25.00 | 2.69 × real time | 0.80 × real time |
| X5 2× 3840² @ 29.97 | 1.18 × real time | 0.67 × real time |
| LRV 1× 1664×832 @ 25.00 | 45.37 × real time | 38.20 × real time |
| LRV 1× 1664×832 @ 29.97 | 56.93 × real time | 47.77 × real time |
| 3840×1920 HEVC 8-bit, synthetic | 284.57 fps | 687.46 fps |
| 5760×2880 HEVC 8-bit, synthetic | 109.96 fps | 294.91 fps |

A second run of both test binaries straight after
([synth](../hw-reports/huginn-tier2-synth.log),
[samples](../hw-reports/huginn-tier2-samples.log)) matched within 10% on
the X5 and LRV rows and on VA-API 3840×1920. The other synthetic rows fell:
VA-API 5760×2880 from 109.96 to 67.30 fps on an idle GPU, software
3840×1920 from 687.46 to 523.08 fps and software 5760×2880 from 294.91 to
243.90 fps. One run per benchmark is not enough to trust those numbers.

Every X5 mode decodes faster than real time on VA-API and slower than real
time in software, except 1920² at 1.08×. 3840² at 29.97 is the tightest
case, at 1.18×. On the synthetic clips, software decode on huginn's CPU beats
VA-API including its copy to RAM.

### odin (RX 7600, Fedora)

Not run yet: no report.

### Framework 13 Pro (Intel Xe3, Arch)

Not available yet.

## Reference: software decode in the cloud workspace

For scale only (4 vCPUs, FFmpeg 6.1.1, not a target machine): the X5 8K30
take decodes in software at about 12 frames per second for one 3840²
stream at a time. Playback needs both streams at 29.97 fps, so this is far
from real time.
