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
| Reframed 4K30 exports at or above real time with hardware encode on odin and huginn | export benchmark (M7/M8) |
| Frame caches are bounded, sized from available memory, configurable | cache tests (M1) |

## How the benchmarks work

Tier 2 tests call `spherecut_testkit::bench::record(name, value, unit)`.
Under `cargo xtask hw-test` each value goes to `target/hw-bench.tsv` and into
the report's Benchmarks table. Benchmarks are skipped with `--quick`, which
builds in debug mode.

Current benchmarks:

- **`bench_decode_hevc_4k_and_5k7`**: decodes 60 frames of a synthetic
  3840×1920 and 5760×2880 HEVC 8-bit clip, in software and on VA-API, in
  frames per second. The VA-API number includes copying each frame to
  system memory, which the render path will avoid later.
- **`bench_samples_realtime_decode`**: for each recording mode in
  `samples/`, decodes up to ten seconds of all its video streams at once,
  one thread per stream as playback does, and reports the slowest stream
  as a multiple of real time (1.0 = just keeps up). An Insta360 X5 8K30
  file has two 3840² streams, so 1.0 means 60 decoded 3840² frames per
  second.

## Results

### huginn (Radeon 8060S, Fedora 44)

No report yet.

### odin (RX 7600, Fedora)

No report yet.

### Framework 13 Pro (Intel Xe3, Arch)

Not available yet.

## Reference: software decode in the cloud workspace

For scale only (4 vCPUs, FFmpeg 6.1.1, not a target machine): the X5 8K30
take decodes in software at about 12 frames per second for one 3840²
stream at a time. Playback needs both streams at 29.97 fps, so this is far
from real time.
