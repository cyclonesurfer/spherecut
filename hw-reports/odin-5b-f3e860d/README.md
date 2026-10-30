# odin: slice 5b zero copy at f3e860d

odin: AMD Radeon RX 7600 (RADV NAVI33), Mesa 26.2.5, FFmpeg 8.1.3, Fedora 44,
kernel 7.2.9-200.fc44, tuned `throughput-performance`. 2026-10-30.

## Doctor (`doctor.json`)

| Vulkan device | device_type | dmabuf_import |
|---|---|---|
| AMD Radeon RX 7600 (RADV NAVI33) | discrete_gpu | true |
| llvmpipe (LLVM 22.1.8, 256 bits) | cpu | true |

## Tier 2

| Test | Result |
|---|---|
| `tier2_playback::zero_copy_matches_copy_path` | pass (16.51 s): hevc-main, hevc-main10 and X5 raw 1920² each 48 frames, the import byte-identical to the copy |
| `tier2_frame::zero_copy_frame_matches_the_copy` | pass (16.40 s): h264, hevc-main and hevc-main10 byte-identical |
| `tier2_playback::bench_x5_8k30_zero_copy_vs_copy` | pass (325.97 s); rows in `bench.tsv` |

`bench_x5_8k30_zero_copy_vs_copy`, VA-API copied / VA-API zero copy / Vulkan Video:

| X5 mode | unthrottled fps | upload_cpu p50 ms | download p50 ms | dropped after 1 s | imports (refused) |
|---|---|---|---|---|---|
| 2× 1920² @ 119.88 | 141.75 / 264.75 / 259.25 | 1.03 / 0.32 / 1.00 | 6.24 / 1.70 / 2.94 | 2252 / 2248 / 2248 (60 Hz display, 0 decode-dropped) | 3579 (0) |
| 2× 2880² @ 59.94 | 66.25 / 135.5 / 135 | 1.67 / 0.31 / 1.67 | 13.94 / 0.02 / 5.93 | 0 / 0 / 0 | 2749 (0) |
| 2× 3840² @ 25.00 | 24.25 / 75.5 / 63 | 2.55 / 0.50 / 3.61 | 22.46 / 0.03 / 13.00 | 0 / 25 (23 decode-dropped) / 0 | 1175 (0) |
| 2× 3840² @ 29.97 | 40.5 / 59 / 78 | 4.27 / 0.95 / 3.82 | 26.03 / 0.02 / 3.76 | 0 / 0 / 0 | 3561 (0) |

## A real 8K frame (`frame-zc.json`, `frame-copy.json`)

`hevc8-8k-30fps-noisy-20s.mp4`, frame 100: `--zero-copy` decoded as
`{"path": "vaapi", "transfer": "drm_prime"}`, `--decoder vaapi` as
`{"path": "vaapi", "transfer": "downloaded"}`. `cmp` finds the two PNGs identical.

## VA-API 8K decode A/B (`ab-*.tsv`, `ab-pre.txt`)

`bench_samples_realtime_decode`, X5 rows, × real time, VA-API / software / Vulkan Video
(median of 3; VA-API range where it is wide). THP `always [madvise] never`, defrag
`[madvise]`, about 14 GiB free and 49 GiB available before every run.

| Run | 1920² @ 119.88 | 2880² @ 59.94 | 3840² @ 25.00 | 3840² @ 29.97 |
|---|---|---|---|---|
| A main-1 (f3e860d) | 1.0601 / 0.7237 / 1.6633 | 0.9913 / 0.6503 / 1.6113 | 0.6054 / 0.5212 / 2.2375 | 0.4643 / 0.4390 / 1.9375 |
| B 43abcda | 1.0723 / 0.7222 / 1.6616 | 0.9888 / 0.6514 / 1.6067 | 1.3208 / 0.5224 / 2.2264 | 0.9313 (0.7728–0.9396) / 0.4388 / 1.9288 |
| A main-2 (f3e860d) | 1.0709 / 0.7242 / 1.6621 | 0.9831 / 0.6501 / 1.6092 | 1.2931 (0.6641–1.3251) / 0.5217 / 2.2435 | 0.4276 (0.4266–0.4911) / 0.4393 / 1.9354 |
| C main, THP off for the process (PR_SET_THP_DISABLE) | 1.0707 / 0.7206 / 1.6637 | 0.9843 / 0.6510 / 1.6100 | 0.6026 / 0.5209 / 2.2176 | 0.4282 / 0.4389 / 1.9358 |

- The VA-API 8K slowdown reproduces at main, and 43abcda is fast in the same machine state.
- In main-2, VA-API 3840² @ 25 had a median of 1.29 but one run of 3 at 0.66. 3840² @ 29.97 stayed slow.
- Disabling THP for the process (C) doesn't restore the speed, so 087987a's MADV_HUGEPAGE is not the cause on odin.
- System AnonHugePages was 0 kB before main-2, before C and after C.
- Only VA-API at 3840² changes. Software and Vulkan Video match across all runs.

The first main-2 attempt didn't build: B's 43abcda build of spherecut-core in the
shared target dir looked current to cargo for main's older-mtime sources (E0432
`DmabufLayer`/`fourcc`, E0407 `dmabuf`). `cargo clean --release -p spherecut-core` fixed
it, and main-2 was rerun. `ab-main-2.log` is the successful rerun.
