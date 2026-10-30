# odin: slice 5b path B at a13ee07

odin: AMD Radeon RX 7600 (RADV NAVI33), Mesa 26.2.5, FFmpeg 8.1.3, Fedora 44,
kernel 7.2.9-200.fc44, tuned `throughput-performance`. 2026-10-30. Run from a
worktree of m1/zero-copy-b at a13ee07 with its own target dir. This branch doesn't
have the VA-API download allocator fix (d8a9054).

## Tier 1

| Test | Result |
|---|---|
| `spherecut-render --lib vkshare` | 7 passed |
| `spherecut-render --test shared_vulkan` | 1 passed (`ffmpegs_vulkan_frames_fill_a_slot_as_their_bytes_do`) |

## Tier 2

| Test | Result |
|---|---|
| `tier2_playback::shared_vulkan_matches_copy_path` | pass (17.27 s): hevc-main, hevc-main10 and X5 raw 1920² each 48 frames, the copy out of FFmpeg's image byte-identical to the copy |
| `tier2_frame::shared_vulkan_frame_matches_the_copy` | pass (17.55 s): h264, hevc-main and hevc-main10 zero copy byte-identical to the copy |
| `tier2_playback::zero_copy_matches_copy_path` (path A) | pass (16.68 s): hevc-main, hevc-main10 and X5 raw 1920² each 48 frames, the import byte-identical to the copy |
| `tier2_playback::bench_x5_8k30_zero_copy_vs_copy` | pass (429.39 s); rows in `bench.tsv` |

The Khronos validation layer isn't installed on odin (no `vulkan-validation-layers`,
and `/usr/share/vulkan/explicit_layer.d/` is empty), so there was no validation run.

## bench_x5_8k30_zero_copy_vs_copy

Unthrottled fps; upload_cpu and upload_gpu p50/p99 in ms; download p50 in ms;
dropped and decode-dropped after 1 s; frames taken on the GPU (refused).

| X5 mode | Path | unthrottled | upload_cpu | upload_gpu | download p50 | dropped / decode-dropped | taken (refused) |
|---|---|---:|---|---|---:|---|---|
| 2× 1920² @ 119.88 | VA-API | 135.75 | 0.82/1.99 | 0.28/0.66 | 6.16 | 2248 / 0 | |
| | VA-API, zero copy | 256 | 0.27/0.57 | 0.40/0.42 | 1.89 | 2248 / 0 | 3577 (0) |
| | Vulkan | 254.5 | 0.83/1.46 | 0.28/0.59 | 3.00 | 2248 / 0 | |
| | Vulkan, zero copy | 276.5 | 0.56/1.02 | 0.35/0.42 | 0.00 | 2248 / 0 | 3573 (0) |
| 2× 2880² @ 59.94 | VA-API | 67.75 | 1.51/3.45 | 0.55/0.87 | 13.77 | 0 / 0 | |
| | VA-API, zero copy | 135.5 | 0.29/0.62 | 0.82/1.12 | 0.01 | 0 / 0 | 2747 (0) |
| | Vulkan | 133 | 1.52/2.43 | 0.56/0.89 | 6.08 | 0 / 0 | |
| | Vulkan, zero copy | 142.5 | 0.76/1.26 | 0.74/1.10 | 0.00 | 0 / 0 | 2747 (0) |
| 2× 3840² @ 25.00 | VA-API | 43 | 2.49/4.84 | 1.70/1.73 | 21.84 | 0 / 0 | |
| | VA-API, zero copy | 75.5 | 0.33/1.37 | 2.07/2.40 | 0.02 | 19 / 19 | 1183 (0) |
| | Vulkan | 74 | 2.50/4.15 | 1.71/2.53 | 9.94 | 0 / 0 | |
| | Vulkan, zero copy | 78 | 1.46/3.70 | 2.06/2.36 | 0.00 | 2 / 2 | 1193 (0) |
| 2× 3840² @ 29.97 | VA-API | 20 | 3.40/5.34 | 0.91/2.75 | 24.55 | 0 / 0 | |
| | VA-API, zero copy | 78 | 0.37/0.87 | 1.55/2.31 | 0.02 | 0 / 0 | 4319 (0) |
| | Vulkan | 78 | 3.92/5.42 | 0.93/1.88 | 3.80 | 0 / 0 | |
| | Vulkan, zero copy | 81 | 1.07/1.57 | 1.35/1.62 | 0.00 | 0 / 0 | 4440 (0) |

- No frame was refused on any mode, on either zero copy path.
- Vulkan zero copy is the fastest unthrottled path in every mode. Its upload_cpu p50 is about half of Vulkan copied (0.56–1.46 against 0.83–3.92 ms), and its download is 0.
- At 3840² @ 25, VA-API zero copy decode-dropped 19 frames (25 at f3e860d), and Vulkan zero copy 2. Every other mode dropped none, apart from the 1920² @ 119.88 drops, which come from the 60 Hz display.
- VA-API copied at 3840² is slow here (43 and 20 fps unthrottled), as expected on a branch without d8a9054.
