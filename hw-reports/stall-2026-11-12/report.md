# odin stall trace on the VA-API copy path (measure only)

## 1. Header

- **Branch and code:** `main` at b0df927 (b0df927). `git diff --stat 0b3ab9b HEAD -- crates xtask` prints nothing. The tree was clean before the trace, with no untracked files.
- **Binary:** `target/release/spherecut` was built by the baseline hw-test at 04:50:37 PST, and no `.rs` file under crates or xtask is newer.
- **env-before.txt:** only `WAYLAND_DISPLAY=wayland-0` and `XDG_RUNTIME_DIR=/run/user/1000`. There are no other SPHERECUT_, LIBVA_, RADV_, MESA_, AMD_, VK_ or GLIBC_ variables.
- **Machine:**
  - Kernel 7.2.9-200.fc44, glibc 2.43, Fedora 44.
  - THP enabled `[madvise]`, defrag `[madvise]`, khugepaged defrag 1; compaction_proactiveness 20.
  - **ptrace_scope 0**, not 1 as the brief assumed.
- **GPU:**
  - dpm `auto`, power profile `BOOTUP_DEFAULT*`.
  - PCIe at all three levels (0c:00.0, 0b:00.0, 0a:00.0): 16.0 GT/s x8, of max 16.0 GT/s x8.
  - pp_dpm_pcie: `0: 2.5GT/s, x1 81Mhz`, `1: 16.0GT/s, x8 306Mhz *`, `2: 16.0GT/s, x8 306Mhz *`.
  - VRAM: vis_vram_total 268435456 (256 MiB) against vram_total 8573157376 (8 GiB), so resizable BAR is off.
- **Packages:** mesa-va-drivers-freeworld-26.2.5-1.fc44; mesa-va-drivers and mesa-vulkan-drivers are not installed as those package names; libva-2.23.0-3; libdrm-2.4.134-1.
- **Tools and driver checks:**
  - strace 7.2: "strace options ok", so `-Y` and `--seccomp-bpf` both work.
  - ffmpeg has the vaapi hwaccel.
  - `novideotiling` is present in libgallium-26.2.5.so (both copies) and in radeonsi_drv_video.so (both), so **arm E ran**.
- **Other apps:** Firefox was running throughout: pid 4119444 and its content process 4120468, each about 16.7 % CPU on average over their 11 h life. Its fdinfo shows client 20222 at about 19.6 % of the gfx engine during the runs (section 5). Nothing else was heavy.
- **Session and present mode:** gnome-session-inhibit was present and running. Every run's present mode, from the "Automatically choosing presentation mode" line, was **Fifo**.
- **Locks:** none. The screen was unlocked before and after every run.
- **Runs:** none was retried or dropped. All 29 runs exited 0 and wrote gui.json, and every run has ui.frames == ui.passes. B's path A ran in all three samples (cache.bytes 0, no "refused" in stderr).
- **Arm D took effect:** decode-v0's minor faults a downloaded frame were 25270 in A against 1417 in D, 17.8×.
- **stderr:** the only WARN in any run is arboard's clipboard fallback, once a run, and there is no ERROR.
- **Not shown in the tables:**
  - G (strace) ran without the sampler, so it is in section 8 only.
  - H is in section 9 only.
  - The S arms' sampler had 288 and 395 late ticks (longest 3.9 and 9.5 ms) under the software decoder's CPU load. Every other run had 1 to 8.

## 2. One row per run

JSON keys: presented `result.ui.presented`; p50/p99 `result.settled.ui.p50/.p99`; hitches `result.ui.hitches`; work p95 `result.ui.work.p95`; covered s = last `late[].at_s` − `settled.from_s`; big = late passes ≥ 95 ms a covered second, mid = 40–95 ms a covered second.

| run | exit | presented | p50 / p99 ms | hitches | work p95 | covered s | big/s | mid/s | dropped |
|---|---|---|---|---|---|---|---|---|---|
| base-0 | – | 1.000 | 16.7 / 17.1 | 0 | 1.19 | 9.0 | 0.00 | 0.00 |  |
| base-1 | – | 0.725 | 16.7 / 116.4 | 114 | 1.70 | 10.6 | 0.85 | 4.06 |  |
| base-2 | – | 0.335 | 50.0 / 51.1 | 6 | 47.28 | 3.2 | 0.00 | 19.98 |  |
| A-1 | 0 | 0.743 | 16.7 / 116.5 | 100 | 1.67 | 12.7 | 1.18 | 2.75 |  |
| A-2 | 0 | 0.733 | 16.7 / 116.4 | 105 | 1.82 | 11.2 | 1.07 | 4.11 |  |
| A-3 | 0 | 0.742 | 16.7 / 100.1 | 107 | 1.64 | 11.9 | 0.76 | 3.88 |  |
| A-end | 0 | 0.782 | 16.7 / 100.0 | 90 | 1.69 | 13.6 | 0.73 | 3.08 |  |
| B-1 | 0 | 0.724 | 16.7 / 83.4 | 194 | 1.53 | 6.1 | 0.16 | 2.79 |  |
| B-2 | 0 | 0.713 | 16.7 / 83.2 | 201 | 1.45 | 5.9 | 0.00 | 4.91 |  |
| B-3 | 0 | 0.777 | 16.7 / 83.3 | 161 | 1.40 | 6.2 | 0.16 | 3.21 |  |
| B2-1 | 0 | 0.918 | 16.7 / 66.8 | 36 | 1.29 | 19.0 | 0.37 | 0.90 |  |
| B2-2 | 0 | 0.920 | 16.7 / 83.2 | 37 | 1.32 | 19.0 | 0.26 | 0.95 |  |
| B2-3 | 0 | 0.904 | 16.7 / 83.3 | 44 | 1.24 | 18.9 | 0.37 | 1.11 |  |
| C-1 | 0 | 0.827 | 16.7 / 66.4 | 166 | 2.54 | 9.6 | 0.00 | 0.62 |  |
| C-2 | 0 | 0.876 | 16.7 / 66.4 | 115 | 2.37 | 13.0 | 0.00 | 1.00 |  |
| C2-1 | 0 | 0.969 | 16.7 / 33.5 | 25 | 1.36 | 19.0 | 0.00 | 0.47 |  |
| C2-2 | 0 | 0.934 | 16.7 / 66.4 | 42 | 1.35 | 19.0 | 0.00 | 1.05 |  |
| C2-3 | 0 | 0.926 | 16.7 / 66.4 | 51 | 1.32 | 19.0 | 0.00 | 1.11 |  |
| D-1 | 0 | 0.762 | 16.7 / 84.1 | 122 | 2.31 | 10.3 | 0.19 | 4.29 |  |
| D-2 | 0 | 0.770 | 16.7 / 84.7 | 118 | 2.56 | 11.0 | 0.18 | 4.01 |  |
| D-3 | 0 | 0.774 | 16.7 / 84.4 | 109 | 2.46 | 12.0 | 0.00 | 4.26 |  |
| E-1 | 0 | 0.760 | 16.7 / 100.1 | 99 | 1.64 | 11.4 | 0.96 | 4.12 |  |
| F-1 | 0 | 0.717 | 16.7 / 116.5 | 114 | 1.80 | 11.2 | 0.98 | 3.92 |  |
| L-1 | 0 | 0.968 | 16.7 / 33.5 | 28 | 2.13 | 19.0 | 0.00 | 0.26 |  |
| L-2 | 0 | 0.994 | 16.7 / 19.1 | 7 | 2.21 | 19.0 | 0.00 | 0.00 |  |
| L-3 | 0 | 0.981 | 16.7 / 33.3 | 19 | 2.17 | 19.0 | 0.00 | 0.11 |  |
| N-1 | 0 | 0.758 | 16.7 / 100.0 | 103 | 1.87 | 13.5 | 0.66 | 3.17 |  |
| N-2 | 0 | 0.732 | 16.7 / 100.2 | 109 | 1.86 | 10.9 | 0.92 | 4.32 |  |
| S-1 | 0 | 0.993 | 16.7 / 19.5 | 9 | 3.17 | 19.0 | 0.00 | 0.00 |  |
| S-2 | 0 | 0.997 | 16.7 / 19.4 | 3 | 3.44 | 19.0 | 0.00 | 0.05 |  |

Stages (`result.stages.*`, ms) and frames:

| run | download n / p50 / p95 | decode p50 / p95 | upload_cpu p50 | upload_gpu p50 | render_gpu p99 | shown | decode_dropped | gate waits / longest ms | cache frames / GB | rss GB | gpu_alloc MB | decoder (fell back from) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base-0 | 200 / 0.0 / 0.0 | 0.3 / 11.4 | 1.0 | 0.7 | 0.1 | 200 | 98 | 11 / 2.04 | 16 / 0.00 | 0.19 | 46 | vulkan (None) |
| base-1 | 129 / 149.2 / 190.7 | 1.0 / 38.6 | 6.0 | 1.9 | 3.0 | 129 | 469 | 8 / 2.17 | 130 / 6.47 | 6.72 | 274 | vaapi (None) |
| base-2 | 200 / 0.0 / 0.0 | 0.3 / 11.4 | 1.8 | 0.7 | 0.2 | 16 | 98 | 84 / 2.14 | 16 / 0.00 | 0.19 | 62 | vulkan (None) |
| A-1 | 124 / 152.5 / 190.9 | 1.1 / 38.2 | 6.1 | 1.9 | 2.3 | 124 | 474 | 11 / 2.00 | 125 / 6.22 | 6.45 | 274 | vaapi (None) |
| A-2 | 128 / 150.3 / 190.3 | 1.1 / 37.5 | 6.1 | 2.0 | 1.6 | 128 | 469 | 13 / 2.02 | 129 / 6.42 | 6.64 | 274 | vaapi (None) |
| A-3 | 131 / 149.1 / 189.7 | 1.0 / 38.3 | 6.1 | 2.0 | 1.8 | 130 | 467 | 4 / 1.91 | 132 / 6.57 | 6.80 | 274 | vaapi (None) |
| A-end | 119 / 151.9 / 190.6 | 1.1 / 78.2 | 6.1 | 1.9 | 3.3 | 119 | 479 | 13 / 2.07 | 120 / 5.97 | 6.18 | 274 | vaapi (None) |
| B-1 | 595 / 0.0 / 19.8 | 0.8 / 1.5 | 0.4 | 1.7 | 0.3 | 522 | 0 | 71 / 2.17 | 16 / 0.00 | 0.27 | 299 | vaapi (None) |
| B-2 | 596 / 0.0 / 22.2 | 0.9 / 1.5 | 0.5 | 1.7 | 0.3 | 519 | 0 | 98 / 2.15 | 16 / 0.00 | 0.27 | 299 | vaapi (None) |
| B-3 | 593 / 0.0 / 16.8 | 0.8 / 1.4 | 0.4 | 1.8 | 0.3 | 546 | 0 | 50 / 2.20 | 16 / 0.00 | 0.27 | 299 | vaapi (None) |
| B2-1 | 66 / 147.5 / 197.9 | 24.6 / 39.2 | 6.0 | 1.9 | 0.3 | 66 | 518 | 6 / 2.10 | 67 / 3.33 | 3.60 | 274 | vaapi (None) |
| B2-2 | 66 / 142.4 / 171.5 | 24.5 / 40.6 | 6.2 | 1.9 | 2.6 | 66 | 522 | 2 / 1.42 | 67 / 3.33 | 3.60 | 274 | vaapi (None) |
| B2-3 | 66 / 152.0 / 195.1 | 27.5 / 48.2 | 5.8 | 1.4 | 0.3 | 66 | 518 | 1 / 0.20 | 67 / 3.33 | 3.69 | 274 | vaapi (None) |
| C-1 | 566 / 5.3 / 46.6 | 1.5 / 18.7 | 6.3 | 1.8 | 1.0 | 551 | 27 | 2 / 1.47 | 172 / 8.56 | 8.96 | 553 | vulkan (None) |
| C-2 | 501 / 5.7 / 56.9 | 1.5 / 18.8 | 6.2 | 1.5 | 1.1 | 485 | 94 | 3 / 1.97 | 172 / 8.56 | 8.95 | 553 | vulkan (None) |
| C2-1 | 131 / 37.1 / 56.7 | 24.6 / 53.3 | 5.3 | 1.9 | 0.3 | 131 | 463 | 4 / 2.16 | 132 / 6.57 | 6.89 | 274 | vulkan (None) |
| C2-2 | 133 / 11.5 / 12.0 | 24.6 / 50.1 | 5.2 | 1.8 | 0.3 | 133 | 460 | 3 / 2.10 | 134 / 6.67 | 7.01 | 274 | vulkan (None) |
| C2-3 | 133 / 11.4 / 11.9 | 24.7 / 49.6 | 5.1 | 1.8 | 0.3 | 133 | 460 | 1 / 1.24 | 134 / 6.67 | 7.01 | 274 | vulkan (None) |
| D-1 | 327 / 45.0 / 110.1 | 1.3 / 3.1 | 6.8 | 2.0 | 1.3 | 327 | 271 | 30 / 2.05 | 21 / 1.05 | 1.40 | 274 | vaapi (None) |
| D-2 | 327 / 46.2 / 106.4 | 1.4 / 3.4 | 6.8 | 2.0 | 1.2 | 327 | 272 | 33 / 2.19 | 21 / 1.05 | 1.42 | 274 | vaapi (None) |
| D-3 | 321 / 46.8 / 108.9 | 1.3 / 3.2 | 6.8 | 2.0 | 1.2 | 321 | 278 | 28 / 2.20 | 21 / 1.05 | 1.40 | 274 | vaapi (None) |
| E-1 | 123 / 151.1 / 180.5 | 1.1 / 40.2 | 6.0 | 1.9 | 0.3 | 123 | 468 | 13 / 2.15 | 124 / 6.17 | 6.48 | 274 | vaapi (None) |
| F-1 | 131 / 150.9 / 190.5 | 1.1 / 37.5 | 6.0 | 1.9 | 1.7 | 131 | 466 | 7 / 2.02 | 132 / 6.57 | 6.80 | 274 | vaapi (None) |
| L-1 | 261 / 69.8 / 102.8 | 1.1 / 2.4 | 6.3 | 2.0 | 1.9 | 261 | 335 | 11 / 2.16 | 172 / 8.56 | 8.93 | 274 | vaapi (None) |
| L-2 | 284 / 69.1 / 86.3 | 1.2 / 2.2 | 6.5 | 2.0 | 1.5 | 283 | 315 | 26 / 2.17 | 172 / 8.56 | 8.88 | 274 | vaapi (None) |
| L-3 | 276 / 69.2 / 91.9 | 1.2 / 3.0 | 6.5 | 2.0 | 2.4 | 275 | 323 | 19 / 2.13 | 172 / 8.56 | 8.91 | 274 | vaapi (None) |
| N-1 | 125 / 150.4 / 178.6 | 1.1 / 46.4 | 6.1 | 1.9 | 0.3 | 125 | 473 | 11 / 2.17 | 126 / 6.27 | 6.51 | 274 | vaapi (None) |
| N-2 | 131 / 148.4 / 190.1 | 1.0 / 38.3 | 6.1 | 1.9 | 2.7 | 131 | 467 | 9 / 2.14 | 132 / 6.57 | 6.77 | 274 | vaapi (None) |
| S-1 | 0 / – / – | 0.4 / 38.5 | 8.6 | 1.4 | 0.1 | 599 | 0 | 0 / 0.00 | 172 / 8.56 | 10.37 | 491 | software (None) |
| S-2 | 0 / – / – | 0.8 / 40.0 | 10.3 | 1.4 | 0.1 | 599 | 0 | 0 / 0.00 | 172 / 8.56 | 10.37 | 491 | software (None) |

/proc/vmstat deltas over the run, PSI after it, decode-v0's minor faults a downloaded frame, the sampler:

| run | pgfault | pgmajfault | compact_stall | compact_fail | compact_success | thp_fault_alloc | thp_fault_fallback | thp_collapse_alloc | allocstall_dma | allocstall_dma32 | allocstall_normal | allocstall_movable | allocstall_device | pgscan_direct | pgmigrate_success | PSI mem / cpu some avg10 | decode-v0 minflt / download | sampler CPU s / late ticks (longest ms) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A-1 | 3208892 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 25319 | 2.41 / 1 (3.477) |
| A-2 | 3378987 | 14 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.07 / 0.0 | 25111 | 2.43 / 3 (4.386) |
| A-3 | 3385613 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.06 / 0.0 | 25300 | 2.46 / 6 (3.137) |
| A-end | 3098304 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.16 | 25350 | 2.40 / 1 (3.352) |
| B-1 | 153548 | 6 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.18 | 24 | 2.27 / 1 (3.049) |
| B-2 | 88070 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 17 | 2.15 / 1 (3.196) |
| B-3 | 89714 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 20 | 2.21 / 1 (3.212) |
| B2-1 | 1794180 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.01 | 26142 | 2.45 / 1 (3.119) |
| B2-2 | 1773325 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.02 | 25782 | 2.42 / 4 (3.067) |
| B2-3 | 1798124 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 26146 | 2.44 / 3 (3.278) |
| C-1 | 2197538 | 1 | 869 | 101 | 768 | 3725 | 427 | 0 | 0 | 0 | 0 | 430 | 0 | 30685 | 598084 | 1.04 / 0.0 | 3762 | 2.33 / 5 (3.267) |
| C-2 | 2184917 | 20 | 313 | 183 | 130 | 3445 | 707 | 0 | 0 | 0 | 0 | 718 | 0 | 55803 | 499481 | 0.87 / 0.0 | 4250 | 2.35 / 1 (3.490) |
| C2-1 | 1700887 | 3 | 2363 | 52 | 2311 | 3057 | 135 | 0 | 0 | 0 | 0 | 137 | 0 | 9692 | 972951 | 1.98 / 0.0 | 12553 | 2.16 / 3 (3.037) |
| C2-2 | 1737755 | 1 | 0 | 0 | 0 | 3264 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 12630 | 2.16 / 1 (3.209) |
| C2-3 | 1728391 | 0 | 0 | 0 | 0 | 3264 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 74827 | 0.0 / 0.0 | 12600 | 2.14 / 2 (3.166) |
| D-1 | 517474 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.16 / 0.0 | 1409 | 2.53 / 7 (3.564) |
| D-2 | 527575 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 1419 | 2.61 / 7 (3.088) |
| D-3 | 518226 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 1423 | 2.71 / 6 (3.084) |
| E-1 | 3237231 | 8 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.12 | 25526 | 2.39 / 3 (3.070) |
| F-1 | 3426153 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 25481 | 2.40 / 1 (3.371) |
| L-1 | 5598936 | 13 | 10 | 2 | 8 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1746 | 0.1 / 0.0 | 20862 | 2.64 / 6 (3.389) |
| L-2 | 5804218 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 20153 | 2.80 / 8 (3.335) |
| L-3 | 5849873 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 70 | 0.0 / 0.08 | 20376 | 2.55 / 2 (3.354) |
| N-1 | 3254617 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 25266 | 2.08 / 1 (3.361) |
| N-2 | 3385266 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 | 25290 | 2.10 / 1 (2.956) |
| S-1 | 2559375 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.93 | – | 3.91 / 288 (3.942) |
| S-2 | 2547571 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 2.0 | – | 3.99 / 395 (9.525) |

D: decode-v0 minflt a downloaded frame, A mean 25270, D mean 1417, A/D 17.8×

Present modes: A-1 Fifo, A-2 Fifo, A-3 Fifo, A-end Fifo, B-1 Fifo, B-2 Fifo, B-3 Fifo, B2-1 Fifo, B2-2 Fifo, B2-3 Fifo, C-1 Fifo, C-2 Fifo, C2-1 Fifo, C2-2 Fifo, C2-3 Fifo, D-1 Fifo, D-2 Fifo, D-3 Fifo, E-1 Fifo, F-1 Fifo, L-1 Fifo, L-2 Fifo, L-3 Fifo, N-1 Fifo, N-2 Fifo, S-1 Fifo, S-2 Fifo

## 3. The main thread's waits

### A-1

Stretch start minus JSON gap start, over 50 gaps of 40 ms or more: median 1.7 ms, p5 -31.2, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 244; of 95 ms or more: 32. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 244 stretches, 12483 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 0.945 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 1.963 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.163 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.697 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.097 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.681 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.081 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.367 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.933 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.931 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.347 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.965 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.163 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.513 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.663 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.963 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.163 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.665 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.945 166 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.129 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.931 180 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.145 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.681 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.947 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.147 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.415 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.915 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.679 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.965 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.165 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.949 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.149 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 80%; R 0 running 20%; S 0 running 0%; R 0 ioctl DRM_SYNCOBJ_WAIT on ? 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%; R 0 futex 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; S 0 running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×72; gnome-shel:cs0 (4584) R 0 ×4; gnome-shell (4536) S 0 ×2; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; gnome-shell (4536) S futex_do_wait ×1; KMS thread (4580) R 0 ×1

### A-2

Stretch start minus JSON gap start, over 58 gaps of 40 ms or more: median 1.6 ms, p5 -29.5, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 238; of 95 ms or more: 30. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 238 stretches, 12266 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 0.946 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 1.948 130 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.130 108 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.932 146 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.130 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.966 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.132 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.944 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.944 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.144 102 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.664 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.964 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.164 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.664 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.964 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.632 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.982 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.166 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.966 110 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.150 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.932 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.316 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.932 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.132 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.932 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.150 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.916 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.698 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.432 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.948 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 79%; R 0 running 21%; S futex_do_wait futex 0%; D lock_mm_and_find_vma -1 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 100%; R 0 running 0%; R 0 futex 0%; S futex_do_wait running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×63; gnome-shell (4536) S 0 ×3; gnome-shel:cs0 (4584) R 0 ×3; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×2; KMS thread (4580) R 0 ×1; KMS thread (4580) S 0 ×1

fdinfo read at run 5.561 s lies within 50 ms of gaps: 5.548+50ms

### A-3

Stretch start minus JSON gap start, over 55 gaps of 40 ms or more: median 1.7 ms, p5 -29.1, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 246; of 95 ms or more: 23. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 245 stretches, 12265 ms
- futex, wchan futex_do_wait: 1 stretches, 48 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 1.965 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.149 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.515 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.945 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.145 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.945 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.529 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.695 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.095 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.697 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.949 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.549 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.549 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.929 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.147 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.963 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.949 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.149 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.983 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.549 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.099 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.949 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.665 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 77%; R 0 running 23%; D drm_exec_lock_contended ioctl AMDGPU_GEM_CREATE on ? 0%; S futex_do_wait futex 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S futex_do_wait running 0%; R 0 futex 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R futex_do_wait futex 0%; S 0 running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×93; gnome-shel:cs0 (4584) R 0 ×7; KMS thread (4580) R 0 ×3; gnome-shell (4536) S futex_do_wait ×3; gnome-shell (4536) S 0 ×1

fdinfo read at run 5.567 s lies within 50 ms of gaps: 5.561+67ms

### A-end

Stretch start minus JSON gap start, over 52 gaps of 40 ms or more: median 1.7 ms, p5 -15.0, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 255; of 95 ms or more: 19. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 255 stretches, 12089 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 2.099 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.099 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.099 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.083 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.667 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.417 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.967 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.119 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.369 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.083 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.969 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.153 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.683 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.933 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.435 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.951 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.551 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.051 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.401 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 79%; R 0 running 20%; S futex_do_wait futex 0%; D drm_exec_lock_contended ioctl AMDGPU_GEM_CREATE on ? 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; S 0 running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×66; KMS thread (4580) R 0 ×25; gnome-shell (4536) S futex_do_wait ×5; gnome-shel:cs0 (4584) R 0 ×3; gnome-shell (4536) S 0 ×3; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×2; KMS thread (4580) S 0 ×2; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1

fdinfo read at run 5.578 s lies within 50 ms of gaps: 5.547+67ms

### B-1

Stretch start minus JSON gap start, over 18 gaps of 40 ms or more: median 2.4 ms, p5 -15.0, p95 3.8.

Main-thread stretches of non-R samples of 25 ms or more: 317; of 95 ms or more: 7. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 317 stretches, 13510 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 1.434 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.334 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.452 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.354 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.452 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.586 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.402 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S futex_do_wait futex 92%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 7%; R 0 running 1%; S 0 running 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R 0 futex 0%; R futex_do_wait futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×79; KMS thread (4580) R 0 ×3; gnome-shell (4536) S 0 ×2; gnome-shell (4536) S futex_do_wait ×1; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1

### B-2

Stretch start minus JSON gap start, over 29 gaps of 40 ms or more: median 1.6 ms, p5 -80.7, p95 2.9.

Main-thread stretches of non-R samples of 25 ms or more: 308; of 95 ms or more: 12. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 308 stretches, 13786 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 2.351 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.651 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.335 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.435 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.335 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.351 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.601 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.301 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.351 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.335 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.335 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.651 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S futex_do_wait futex 89%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 10%; R 0 running 1%; S 0 running 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R futex_do_wait futex 0%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×124; KMS thread (4580) R 0 ×17; gnome-shell (4536) S futex_do_wait ×12; gnome-shel:cs0 (4584) R 0 ×10; KMS thread (4580) S 0 ×6; gnome-shell (4536) S 0 ×5; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×2; gnome-shel:cs0 (4584) S 0 ×1

fdinfo read at run 5.716 s lies within 50 ms of gaps: 5.633+50ms

### B-3

Stretch start minus JSON gap start, over 21 gaps of 40 ms or more: median 1.8 ms, p5 -15.0, p95 3.1.

Main-thread stretches of non-R samples of 25 ms or more: 315; of 95 ms or more: 9. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 315 stretches, 13362 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 1.335 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 1.435 182 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.319 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.519 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.335 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.519 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.487 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.437 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.703 148 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S futex_do_wait futex 92%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 8%; R 0 running 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 futex 0%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R 0 futex 0%; R futex_do_wait futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×48; KMS thread (4580) R 0 ×19; KMS thread (4580) S 0 ×2; gnome-shell (4536) S futex_do_wait ×1; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1

### B2-1

Stretch start minus JSON gap start, over 24 gaps of 40 ms or more: median 1.7 ms, p5 -15.1, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 312; of 95 ms or more: 8. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 312 stretches, 12312 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 2.733 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.033 182 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.383 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.751 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.321 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.987 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.999 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.001 116 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 53%; S hrtimer_nanosleep 230 29%; R 0 running 17%; S hrtimer_nanosleep running 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×58; gnome-shell (4536) S futex_do_wait ×3; KMS thread (4580) R 0 ×2; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1; gnome-shell (4536) S 0 ×1; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; gnome-shel:cs0 (4584) R 0 ×1

### B2-2

Stretch start minus JSON gap start, over 23 gaps of 40 ms or more: median 1.9 ms, p5 -28.7, p95 2.7.

Main-thread stretches of non-R samples of 25 ms or more: 304; of 95 ms or more: 10. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 304 stretches, 11879 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 2.750 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.034 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.466 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.139 128 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.168 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.352 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.650 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.051 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.330 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.880 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 52%; S hrtimer_nanosleep 230 31%; R 0 running 17%; S 0 running 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×63; KMS thread (4580) R 0 ×3; gnome-shel:cs0 (4584) R 0 ×2; gnome-shell (4536) S futex_do_wait ×1

fdinfo read at run 5.577 s lies within 50 ms of gaps: 5.464+100ms

fdinfo read at run 15.577 s lies within 50 ms of gaps: 15.496+67ms

### B2-3

Stretch start minus JSON gap start, over 28 gaps of 40 ms or more: median 2.0 ms, p5 -31.2, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 319; of 95 ms or more: 10. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 319 stretches, 12512 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 0.984 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.366 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.000 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.750 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.368 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.316 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.966 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.014 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.214 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.998 114 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 61%; R 0 running 20%; S hrtimer_nanosleep 230 19%; R 0 230 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; S 0 running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×61; gnome-shel:cs0 (4584) R 0 ×3; gnome-shell (4536) S futex_do_wait ×3; KMS thread (4580) R 0 ×2; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; gnome-shell (4536) S 0 ×1; gnome-shel:cs0 (4584) R futex_do_wait ×1

### C-1

Stretch start minus JSON gap start, over 6 gaps of 40 ms or more: median 2.0 ms, p5 -13.2, p95 2.3.

Main-thread stretches of non-R samples of 25 ms or more: 270; of 95 ms or more: 2. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 269 stretches, 9391 ms
- futex, wchan futex_do_wait: 1 stretches, 30 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 11.811 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.829 96 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S futex_do_wait futex 66%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 18%; R 0 running 10%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_TIMELINE_WAIT on ? 6%
- spherecut:cs0: S futex_do_wait futex 100%
- upload: S futex_do_wait futex 93%; R 0 running 7%; D do_mprotect_pkey 10 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×42; KMS thread (4580) R 0 ×3; gnome-shel:cs0 (4584) R 0 ×2; KMS thread (4580) S 0 ×1; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; gnome-shell (4536) S futex_do_wait ×1

### C-2

Stretch start minus JSON gap start, over 13 gaps of 40 ms or more: median 1.6 ms, p5 -13.7, p95 2.7.

Main-thread stretches of non-R samples of 25 ms or more: 252; of 95 ms or more: 0. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 248 stretches, 8744 ms
- 10, wchan do_mprotect_pkey: 1 stretches, 64 ms
- futex, wchan futex_do_wait: 2 stretches, 62 ms
- ioctl AMDGPU_CS on /dev/dri/renderD128, wchan amdgpu_cs_submit: 1 stretches, 56 ms

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S futex_do_wait futex 52%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_TIMELINE_WAIT on ? 19%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 14%; R 0 running 13%
- spherecut:cs0: S futex_do_wait futex 100%
- upload: S futex_do_wait futex 93%; R 0 running 7%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×23; gnome-shell (4536) S futex_do_wait ×2; KMS thread (4580) R 0 ×2; gnome-shel:cs0 (4584) D amdgpu_cs_submit ×1

### C2-1

Stretch start minus JSON gap start, over 9 gaps of 40 ms or more: median -6.7 ms, p5 -15.0, p95 2.7.

Main-thread stretches of non-R samples of 25 ms or more: 277; of 95 ms or more: 0. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 276 stretches, 9897 ms
- futex, wchan futex_do_wait: 1 stretches, 28 ms

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S hrtimer_nanosleep 230 43%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_TIMELINE_WAIT on ? 37%; R 0 running 13%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 8%
- upload: S futex_do_wait futex 97%; R 0 running 3%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×41; KMS thread (4580) R 0 ×17; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×2; gnome-shel:cs0 (4584) R 0 ×1; gnome-shell (4536) R futex_do_wait ×1; gnome-shell (4536) S 0 ×1; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1; gnome-shel:cs0 (4584) S 0 ×1

### C2-2

Stretch start minus JSON gap start, over 20 gaps of 40 ms or more: median 1.6 ms, p5 -31.1, p95 2.5.

Main-thread stretches of non-R samples of 25 ms or more: 313; of 95 ms or more: 7. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 312 stretches, 12059 ms
- futex, wchan futex_do_wait: 1 stretches, 28 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 3.950 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.016 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.516 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.050 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.150 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.386 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.970 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S hrtimer_nanosleep 230 49%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_TIMELINE_WAIT on ? 38%; R 0 running 8%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 5%
- spherecut:cs0: S futex_do_wait futex 98%; R 0 running 2%
- upload: S futex_do_wait futex 95%; R 0 running 5%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×51; gnome-shel:cs0 (4584) R 0 ×6; KMS thread (4580) R 0 ×2; gnome-shell (4536) S 0 ×1; gnome-shell (4536) S futex_do_wait ×1

### C2-3

Stretch start minus JSON gap start, over 21 gaps of 40 ms or more: median 1.7 ms, p5 -15.1, p95 2.3.

Main-thread stretches of non-R samples of 25 ms or more: 311; of 95 ms or more: 3. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 310 stretches, 11592 ms
- futex, wchan futex_do_wait: 1 stretches, 36 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 6.973 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.155 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.937 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S hrtimer_nanosleep 230 43%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_TIMELINE_WAIT on ? 41%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 8%; R 0 running 8%
- spherecut:cs0: S futex_do_wait futex 100%
- upload: S futex_do_wait futex 95%; R 0 running 5%; S 0 running 0%; R futex_do_wait futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×50; gnome-shel:cs0 (4584) R 0 ×6; gnome-shell (4536) S futex_do_wait ×2; KMS thread (4580) R 0 ×1; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1; gnome-shel:cs0 (4584) R futex_do_wait ×1; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1

### D-1

Stretch start minus JSON gap start, over 46 gaps of 40 ms or more: median 1.8 ms, p5 -15.1, p95 2.8.

Main-thread stretches of non-R samples of 25 ms or more: 219; of 95 ms or more: 8. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 218 stretches, 10258 ms
- futex, wchan futex_do_wait: 1 stretches, 32 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 1.968 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.382 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.914 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.014 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.884 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.864 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.014 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.000 114 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 79%; R 0 running 20%; S futex_do_wait futex 0%; S 0 running 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%; R 0 futex 0%
- upload: S futex_do_wait futex 95%; R 0 running 5%; R 0 futex 0%; R futex_do_wait futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×67; KMS thread (4580) R 0 ×14; gnome-shel:cs0 (4584) R 0 ×5; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; gnome-shell (4536) S futex_do_wait ×1; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1

### D-2

Stretch start minus JSON gap start, over 46 gaps of 40 ms or more: median 1.8 ms, p5 -14.6, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 216; of 95 ms or more: 8. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 215 stretches, 10135 ms
- futex, wchan futex_do_wait: 1 stretches, 32 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 3.910 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.928 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.978 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.012 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.862 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.846 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.994 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.646 96 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 80%; R 0 running 20%; S futex_do_wait futex 1%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; R 0 futex 0%
- upload: S futex_do_wait futex 94%; R 0 running 6%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×83; gnome-shel:cs0 (4584) R 0 ×6; gnome-shell (4536) S futex_do_wait ×4; KMS thread (4580) R 0 ×2; gnome-shel:cs0 (4584) R futex_do_wait ×1; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; KMS thread (4580) S 0 ×1; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1

### D-3

Stretch start minus JSON gap start, over 51 gaps of 40 ms or more: median 1.8 ms, p5 -15.0, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 207; of 95 ms or more: 7. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 207 stretches, 10048 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 7.077 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.513 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.143 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.845 112 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.897 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.125 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.993 116 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 82%; R 0 running 18%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%
- upload: S futex_do_wait futex 95%; R 0 running 5%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×65; gnome-shell (4536) S futex_do_wait ×2; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×2; KMS thread (4580) S 0 ×1; gnome-shell (4536) S 0 ×1; KMS thread (4580) R 0 ×1

### E-1

Stretch start minus JSON gap start, over 58 gaps of 40 ms or more: median 1.7 ms, p5 -15.1, p95 2.5.

Main-thread stretches of non-R samples of 25 ms or more: 264; of 95 ms or more: 25. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 263 stretches, 12929 ms
- 230, wchan hrtimer_nanosleep: 1 stretches, 70 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 0.931 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 1.531 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.081 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.947 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.147 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.947 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.147 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.981 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.147 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.681 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.963 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.693 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.095 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.679 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.979 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.163 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.679 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.697 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 11.933 130 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.931 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.131 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.281 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.431 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.665 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.433 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 75%; R 0 running 25%; S futex_do_wait futex 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×75; KMS thread (4580) S 0 ×11; KMS thread (4580) R 0 ×10; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×9; gnome-shell (4536) S 0 ×4; gnome-shel:cs0 (4584) R 0 ×3; gnome-shell (4536) S futex_do_wait ×2

fdinfo read at run 5.560 s lies within 50 ms of gaps: 5.545+67ms

### F-1

Stretch start minus JSON gap start, over 55 gaps of 40 ms or more: median 1.6 ms, p5 -15.1, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 241; of 95 ms or more: 27. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 241 stretches, 12633 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 1.946 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.146 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.964 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.130 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.962 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.128 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.828 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.962 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.944 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.144 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.944 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.144 134 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.648 130 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.962 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.162 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.930 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.298 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.962 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.282 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.948 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.132 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.928 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.278 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.928 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.294 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.944 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.144 102 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 78%; R 0 running 22%; S futex_do_wait futex 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%
- upload: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×72; KMS thread (4580) R 0 ×2; gnome-shel:cs0 (4584) R 0 ×2; gnome-shell (4536) R poll_schedule_timeout.constprop.0 ×1

### L-1

Stretch start minus JSON gap start, over 5 gaps of 40 ms or more: median 1.8 ms, p5 -14.8, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 255; of 95 ms or more: 0. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 254 stretches, 8813 ms
- futex, wchan futex_do_wait: 1 stretches, 44 ms

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: R 0 running 54%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 45%; S futex_do_wait futex 0%; D drm_exec_lock_contended ioctl AMDGPU_GEM_CREATE on ? 0%
- spherecut:cs0: S futex_do_wait futex 99%; R 0 running 0%; S 0 running 0%
- upload: S futex_do_wait futex 94%; R 0 running 6%; S ttm_bo_vm_reserve -1 0%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×24; gnome-shel:cs0 (4584) R 0 ×2; gnome-shel:cs0 (4584) S 0 ×1; gnome-shell (4536) S futex_do_wait ×1

### L-2

Main-thread stretches of non-R samples of 25 ms or more: 215; of 95 ms or more: 0. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 215 stretches, 6813 ms

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: R 0 running 84%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 16%
- spherecut:cs0: S futex_do_wait futex 100%
- upload: S futex_do_wait futex 91%; R 0 running 9%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×1

### L-3

Stretch start minus JSON gap start, over 2 gaps of 40 ms or more: median -6.1 ms, p5 -14.6, p95 2.5.

Main-thread stretches of non-R samples of 25 ms or more: 210; of 95 ms or more: 0. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 210 stretches, 7047 ms

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: R 0 running 68%; S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 31%; D drm_exec_lock_contended ioctl AMDGPU_GEM_CREATE on ? 1%; S futex_do_wait futex 0%
- spherecut:cs0: S futex_do_wait futex 98%; R 0 running 1%; S drm_exec_lock_obj ioctl AMDGPU_CS on ? 1%
- upload: S futex_do_wait futex 90%; R 0 running 10%; S dma_fence_default_wait -1 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×17; KMS thread (4580) S 0 ×1; gnome-shell (4536) S 0 ×1

### N-1

Stretch start minus JSON gap start, over 52 gaps of 40 ms or more: median 1.9 ms, p5 -31.1, p95 2.6.

Main-thread stretches of non-R samples of 25 ms or more: 268; of 95 ms or more: 20. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 267 stretches, 12964 ms
- futex, wchan futex_do_wait: 1 stretches, 30 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 0.932 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 1.966 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.518 110 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.114 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.932 182 epoll_pwait on anon_inode:[eventpoll] ep_poll; 4.532 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 5.666 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.100 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.666 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.950 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.950 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.168 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.684 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.278 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.948 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.696 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.966 114 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.946 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.928 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.662 98 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 78%; R 0 running 22%; S futex_do_wait futex 0%; R 0 ioctl DRM_SYNCOBJ_WAIT on ? 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R 0 futex 0%; S futex_do_wait running 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×79; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×2; KMS thread (4580) S 0 ×1; JS Helper (4554) R 0 ×1; gnome-shell (4536) S 0 ×1; KMS thread (4580) R 0 ×1; gnome-shell (4536) S futex_do_wait ×1; gnome-shel:cs0 (4584) R 0 ×1

fdinfo read at run 5.589 s lies within 50 ms of gaps: 5.564+50ms

### N-2

Stretch start minus JSON gap start, over 57 gaps of 40 ms or more: median 1.8 ms, p5 -29.1, p95 2.5.

Main-thread stretches of non-R samples of 25 ms or more: 246; of 95 ms or more: 29. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 246 stretches, 12665 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 1.003 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 1.951 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 2.817 100 epoll_pwait on anon_inode:[eventpoll] ep_poll; 3.951 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.417 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 6.951 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.167 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.551 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 7.969 120 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.169 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 8.969 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.153 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 9.803 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.137 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.271 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 10.935 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.249 96 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.397 148 epoll_pwait on anon_inode:[eventpoll] ep_poll; 12.949 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.383 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 13.767 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.667 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 14.969 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 15.151 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 16.401 132 epoll_pwait on anon_inode:[eventpoll] ep_poll; 17.959 136 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.147 116 epoll_pwait on anon_inode:[eventpoll] ep_poll; 18.933 98 epoll_pwait on anon_inode:[eventpoll] ep_poll; 19.667 114 epoll_pwait on anon_inode:[eventpoll] ep_poll

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: S drm_syncobj_array_wait_timeout.constprop.0 ioctl DRM_SYNCOBJ_WAIT on ? 76%; R 0 running 23%; S futex_do_wait futex 0%; R 0 futex 0%
- spherecut:cs0: S futex_do_wait futex 100%; R 0 running 0%; S 0 running 0%; R futex_do_wait futex 0%
- upload: S futex_do_wait futex 99%; R 0 running 1%; R futex_do_wait futex 0%; R 0 futex 0%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×60; gnome-shell (4536) S futex_do_wait ×4; KMS thread (4580) R poll_schedule_timeout.constprop.0 ×1; KMS thread (4580) S 0 ×1; gnome-shell (4536) S 0 ×1

fdinfo read at run 5.586 s lies within 50 ms of gaps: 5.548+67ms

### S-1

Main-thread stretches of non-R samples of 25 ms or more: 60; of 95 ms or more: 1. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 57 stretches, 1798 ms
- 230, wchan hrtimer_nanosleep: 1 stretches, 128 ms
- futex, wchan futex_do_wait: 2 stretches, 55 ms

Stretches of 95 ms or more (run s, ms, syscall, wchan): 20.144 128 230 hrtimer_nanosleep

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: R 0 running 54%; S futex_do_wait futex 45%; D __vm_munmap munmap 1%
- spherecut:cs0: S futex_do_wait futex 90%; R 0 running 10%
- upload: S futex_do_wait futex 85%; D __vm_munmap munmap 12%; R 0 running 4%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×5; KMS thread (4580) S 0 ×1

### S-2

Stretch start minus JSON gap start, over 1 gaps of 40 ms or more: median 5.3 ms, p5 5.3, p95 5.3.

Main-thread stretches of non-R samples of 25 ms or more: 45; of 95 ms or more: 0. By syscall and wchan (count, total ms):

- epoll_pwait on anon_inode:[eventpoll], wchan ep_poll: 41 stretches, 1299 ms
- 230, wchan hrtimer_nanosleep: 2 stretches, 138 ms
- futex, wchan futex_do_wait: 2 stretches, 74 ms

During main-thread waits of 40 ms or more, the other threads (samples):

- decode-v0: R 0 running 51%; S futex_do_wait futex 48%; D __vm_munmap munmap 1%
- upload: D __vm_munmap munmap 41%; S futex_do_wait futex 36%; R 0 running 20%; D 0 munmap 1%

gnome-shell threads off their usual wait during those main-thread waits (10 ms samples): gnome-shell (4536) R 0 ×2

### Summed over all sampler runs

JSON gaps of 95 ms or more: 112; their main-thread milliseconds by kind:

- epoll_pwait (anon_inode:[eventpoll]): 12182 ms
- runnable (R): 176 ms
- other: 6 ms

Gaps with 50 ms or more inside AMDGPU_CS: 0.

The 40 to 95 ms gaps, summed:

- epoll_pwait (anon_inode:[eventpoll]): 42562 ms
- runnable (R): 1154 ms
- other: 78 ms
- AMDGPU_CS: 54 ms
- futex: 18 ms

All main-thread wait stretches, every sampler run, 5 ms bins (the 200 bin is 200 ms or more):

| bin ms | 0 | 5 | 10 | 15 | 20 | 25 | 30 | 35 | 40 | 45 | 50 | 55 | 60 | 65 | 70 | 75 | 80 | 85 | 90 | 95 | 100 | 105 | 110 | 115 | 125 | 130 | 135 | 145 | 165 | 175 | 180 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| stretches | 493 | 641 | 11230 | 3265 | 111 | 725 | 3793 | 25 | 35 | 1031 | 47 | 11 | 234 | 153 | 9 | 40 | 320 | 1 | 6 | 164 | 4 | 3 | 21 | 23 | 5 | 51 | 1 | 20 | 1 | 1 | 3 |

## 4. Media timing

- B: 439 packets, mean 0.20 MB, max 0.94 MB
- CRA (K): 20 packets, mean 2.53 MB, max 6.02 MB
- P: 141 packets, mean 0.83 MB, max 1.40 MB

CRA pictures (dts, pts in media s): -0.067/0.000, 0.900/1.000, 1.900/2.000, 2.900/3.000, 3.900/4.000, 4.900/5.000, 5.900/6.000, 6.900/7.000, 7.900/8.000, 8.900/9.000, 9.900/10.000, 10.900/11.000, 11.900/12.000, 12.900/13.000, 13.900/14.000, 14.900/15.000, 15.900/16.000, 16.900/17.000, 17.900/18.000, 18.900/19.000

Media time is taken as run time (the run plays from media 0 at run 0).

Gaps of 95 ms or more starting 0–60 ms after a CRA's dts: 15 of 112; after its pts: 5 of 112.

<details><summary>each gap</summary>

- A-1 1.961s: +61 ms from CRA dts, -39 ms from CRA pts
- A-1 2.161s: +261 ms from CRA dts, +161 ms from CRA pts
- A-1 3.095s: +195 ms from CRA dts, +95 ms from CRA pts
- A-1 3.696s: -204 ms from CRA dts, -304 ms from CRA pts
- A-1 4.113s: +213 ms from CRA dts, +113 ms from CRA pts
- A-1 4.365s: +465 ms from CRA dts, +365 ms from CRA pts
- A-1 6.963s: +63 ms from CRA dts, -37 ms from CRA pts
- A-1 7.162s: +262 ms from CRA dts, +162 ms from CRA pts
- A-1 7.678s: -222 ms from CRA dts, -322 ms from CRA pts
- A-1 7.978s: +78 ms from CRA dts, -22 ms from CRA pts
- A-1 8.961s: +61 ms from CRA dts, -39 ms from CRA pts
- A-1 9.960s: +60 ms from CRA dts, -40 ms from CRA pts
- A-1 10.962s: +62 ms from CRA dts, -38 ms from CRA pts
- A-1 12.413s: -487 ms from CRA dts, +413 ms from CRA pts
- A-1 12.930s: +30 ms from CRA dts, -70 ms from CRA pts
- A-2 1.962s: +62 ms from CRA dts, -38 ms from CRA pts
- A-2 2.962s: +62 ms from CRA dts, -38 ms from CRA pts
- A-2 3.145s: +245 ms from CRA dts, +145 ms from CRA pts
- A-2 4.964s: +64 ms from CRA dts, -36 ms from CRA pts
- A-2 7.960s: +60 ms from CRA dts, -40 ms from CRA pts
- A-2 8.962s: +62 ms from CRA dts, -38 ms from CRA pts
- A-2 9.162s: +262 ms from CRA dts, +162 ms from CRA pts
- A-2 9.963s: +63 ms from CRA dts, -37 ms from CRA pts
- A-2 10.981s: +81 ms from CRA dts, -19 ms from CRA pts
- A-2 11.165s: +265 ms from CRA dts, +165 ms from CRA pts
- A-2 11.965s: +65 ms from CRA dts, -35 ms from CRA pts
- A-2 12.148s: +248 ms from CRA dts, +148 ms from CRA pts
- A-3 1.963s: +63 ms from CRA dts, -37 ms from CRA pts
- A-3 2.147s: +247 ms from CRA dts, +147 ms from CRA pts
- A-3 2.960s: +60 ms from CRA dts, -40 ms from CRA pts
- A-3 3.160s: +260 ms from CRA dts, +160 ms from CRA pts
- A-3 5.694s: -206 ms from CRA dts, -306 ms from CRA pts
- A-3 6.094s: +194 ms from CRA dts, +94 ms from CRA pts
- A-3 10.947s: +47 ms from CRA dts, -53 ms from CRA pts
- A-3 11.548s: -352 ms from CRA dts, -452 ms from CRA pts
- A-3 12.547s: -353 ms from CRA dts, -453 ms from CRA pts
- A-end 2.097s: +197 ms from CRA dts, +97 ms from CRA pts
- A-end 3.097s: +197 ms from CRA dts, +97 ms from CRA pts
- A-end 4.097s: +197 ms from CRA dts, +97 ms from CRA pts
- A-end 7.098s: +198 ms from CRA dts, +98 ms from CRA pts
- A-end 7.682s: -218 ms from CRA dts, -318 ms from CRA pts
- A-end 10.117s: +217 ms from CRA dts, +117 ms from CRA pts
- A-end 10.367s: +467 ms from CRA dts, +367 ms from CRA pts
- A-end 12.098s: +198 ms from CRA dts, +98 ms from CRA pts
- A-end 12.967s: +67 ms from CRA dts, -33 ms from CRA pts
- A-end 13.151s: +251 ms from CRA dts, +151 ms from CRA pts
- B-1 1.449s: -451 ms from CRA dts, +449 ms from CRA pts
- B-3 1.450s: -450 ms from CRA dts, +450 ms from CRA pts
- B2-1 3.047s: +147 ms from CRA dts, +47 ms from CRA pts
- B2-1 3.397s: +497 ms from CRA dts, +397 ms from CRA pts
- B2-1 5.765s: -135 ms from CRA dts, -235 ms from CRA pts
- B2-1 9.318s: +418 ms from CRA dts, +318 ms from CRA pts
- B2-1 10.001s: +101 ms from CRA dts, +1 ms from CRA pts
- B2-1 12.996s: +96 ms from CRA dts, -4 ms from CRA pts
- B2-1 19.016s: +116 ms from CRA dts, +16 ms from CRA pts
- B2-2 4.032s: +132 ms from CRA dts, +32 ms from CRA pts
- B2-2 5.464s: -436 ms from CRA dts, +464 ms from CRA pts
- B2-2 6.149s: +249 ms from CRA dts, +149 ms from CRA pts
- B2-2 9.167s: +267 ms from CRA dts, +167 ms from CRA pts
- B2-2 17.345s: +445 ms from CRA dts, +345 ms from CRA pts
- B2-3 3.364s: +464 ms from CRA dts, +364 ms from CRA pts
- B2-3 6.998s: +98 ms from CRA dts, -2 ms from CRA pts
- B2-3 8.765s: -135 ms from CRA dts, -235 ms from CRA pts
- B2-3 12.314s: +414 ms from CRA dts, +314 ms from CRA pts
- B2-3 12.997s: +97 ms from CRA dts, -3 ms from CRA pts
- B2-3 16.012s: +112 ms from CRA dts, +12 ms from CRA pts
- B2-3 18.996s: +96 ms from CRA dts, -4 ms from CRA pts
- D-1 1.966s: +66 ms from CRA dts, -34 ms from CRA pts
- D-1 4.913s: +13 ms from CRA dts, -87 ms from CRA pts
- D-2 3.909s: +9 ms from CRA dts, -91 ms from CRA pts
- D-2 5.925s: +25 ms from CRA dts, -75 ms from CRA pts
- E-1 2.095s: +195 ms from CRA dts, +95 ms from CRA pts
- E-1 2.961s: +61 ms from CRA dts, -39 ms from CRA pts
- E-1 3.145s: +245 ms from CRA dts, +145 ms from CRA pts
- E-1 3.962s: +62 ms from CRA dts, -38 ms from CRA pts
- E-1 4.978s: +78 ms from CRA dts, -22 ms from CRA pts
- E-1 5.161s: +261 ms from CRA dts, +161 ms from CRA pts
- E-1 7.690s: -210 ms from CRA dts, -310 ms from CRA pts
- E-1 8.092s: +192 ms from CRA dts, +92 ms from CRA pts
- E-1 8.676s: -224 ms from CRA dts, -324 ms from CRA pts
- E-1 8.977s: +77 ms from CRA dts, -23 ms from CRA pts
- E-1 9.160s: +260 ms from CRA dts, +160 ms from CRA pts
- F-1 1.961s: +61 ms from CRA dts, -39 ms from CRA pts
- F-1 2.962s: +62 ms from CRA dts, -38 ms from CRA pts
- F-1 3.145s: +245 ms from CRA dts, +145 ms from CRA pts
- F-1 4.960s: +60 ms from CRA dts, -40 ms from CRA pts
- F-1 6.960s: +60 ms from CRA dts, -40 ms from CRA pts
- F-1 7.959s: +59 ms from CRA dts, -41 ms from CRA pts
- F-1 8.959s: +59 ms from CRA dts, -41 ms from CRA pts
- F-1 9.159s: +259 ms from CRA dts, +159 ms from CRA pts
- F-1 9.678s: -222 ms from CRA dts, -322 ms from CRA pts
- F-1 9.977s: +77 ms from CRA dts, -23 ms from CRA pts
- F-1 10.945s: +45 ms from CRA dts, -55 ms from CRA pts
- N-1 3.112s: +212 ms from CRA dts, +112 ms from CRA pts
- N-1 3.963s: +63 ms from CRA dts, -37 ms from CRA pts
- N-1 6.098s: +198 ms from CRA dts, +98 ms from CRA pts
- N-1 6.681s: -219 ms from CRA dts, -319 ms from CRA pts
- N-1 8.949s: +49 ms from CRA dts, -51 ms from CRA pts
- N-1 9.166s: +266 ms from CRA dts, +166 ms from CRA pts
- N-1 9.682s: -218 ms from CRA dts, -318 ms from CRA pts
- N-1 12.293s: +393 ms from CRA dts, +293 ms from CRA pts
- N-1 12.946s: +46 ms from CRA dts, -54 ms from CRA pts
- N-2 2.815s: -85 ms from CRA dts, -185 ms from CRA pts
- N-2 3.948s: +48 ms from CRA dts, -52 ms from CRA pts
- N-2 6.432s: -468 ms from CRA dts, +432 ms from CRA pts
- N-2 6.949s: +49 ms from CRA dts, -51 ms from CRA pts
- N-2 7.165s: +265 ms from CRA dts, +165 ms from CRA pts
- N-2 7.967s: +67 ms from CRA dts, -33 ms from CRA pts
- N-2 8.167s: +267 ms from CRA dts, +167 ms from CRA pts
- N-2 8.967s: +67 ms from CRA dts, -33 ms from CRA pts
- N-2 9.151s: +251 ms from CRA dts, +151 ms from CRA pts
- N-2 10.950s: +50 ms from CRA dts, -50 ms from CRA pts

</details>

A's 40–95 ms gaps against the nearest P picture's dts: 169 gaps, offset median +49 ms, p5 -54, p95 +65.

## 5. fdinfo (6 s and 16 s)

### A-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 610739: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.71% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.71% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.71% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| spherecut 610739 fd 24 | 23395 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 610739 fd 19 | 23395 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 610739 fd 18 | 23395 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 610739 fd 9 | 23393 | gfx 57.18%, compute 30.11%, enc 757.34% | vram 1052328 KiB, gtt 256260 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183400 KiB, requested-gtt 125188 KiB |

spherecut client ids with gfx and enc/dec both rising: 23393

### A-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 612325: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.27% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.27% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.27% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 612325 fd 25 | 23490 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612325 fd 18 | 23490 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612325 fd 17 | 23490 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612325 fd 9 | 23488 | gfx 59.17%, compute 30.80%, enc 751.75% | vram 1056416 KiB, gtt 256292 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1187488 KiB, requested-gtt 125220 KiB |

spherecut client ids with gfx and enc/dec both rising: 23488

### A-3

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 613881: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 20.48% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 20.50% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 20.50% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| spherecut 613881 fd 23 | 23585 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613881 fd 19 | 23585 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613881 fd 18 | 23585 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613881 fd 9 | 23583 | gfx 59.96%, compute 31.42%, enc 746.68% | vram 1056404 KiB, gtt 256264 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1187476 KiB, requested-gtt 125192 KiB |

spherecut client ids with gfx and enc/dec both rising: 23583

### A-end

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 617689: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.34% | vram 243492 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243492 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.34% | vram 243492 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243492 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.34% | vram 243492 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243492 KiB, requested-gtt 12304 KiB |
| spherecut 617689 fd 23 | 23746 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 617689 fd 18 | 23746 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 617689 fd 17 | 23746 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 617689 fd 9 | 23744 | gfx 53.83%, compute 28.26%, enc 771.47% | vram 1056404 KiB, gtt 258332 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1187476 KiB, requested-gtt 127260 KiB |

spherecut client ids with gfx and enc/dec both rising: 23744

### B-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 611099: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.55% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.55% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.55% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 611099 fd 25 | 23417 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611099 fd 19 | 23417 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611099 fd 18 | 23417 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611099 fd 9 | 23415 | gfx 62.32%, enc 810.55% | vram 1827560 KiB, gtt 94748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1827560 KiB, requested-gtt 94748 KiB |

spherecut client ids with gfx and enc/dec both rising: 23415

### B-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 612525: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.41% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.41% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.41% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 612525 fd 23 | 23501 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612525 fd 19 | 23501 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612525 fd 18 | 23501 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612525 fd 9 | 23499 | gfx 63.46%, enc 818.81% | vram 1827552 KiB, gtt 94748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1827552 KiB, requested-gtt 94748 KiB |

spherecut client ids with gfx and enc/dec both rising: 23499

### B-3

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 614046: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.46% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.46% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.46% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| spherecut 614046 fd 24 | 23596 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614046 fd 18 | 23596 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614046 fd 17 | 23596 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614046 fd 9 | 23594 | gfx 64.47%, enc 744.96% | vram 1827576 KiB, gtt 94748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1827576 KiB, requested-gtt 94748 KiB |

spherecut client ids with gfx and enc/dec both rising: 23594

### B2-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 611341: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.65% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.65% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.65% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 611341 fd 25 | 23428 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611341 fd 18 | 23428 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611341 fd 17 | 23428 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611341 fd 9 | 23426 | gfx 34.93%, compute 16.79%, enc 555.49% | vram 1052308 KiB, gtt 304880 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183380 KiB, requested-gtt 173808 KiB |

spherecut client ids with gfx and enc/dec both rising: 23426

### B2-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 612702: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.30% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.30% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.30% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| spherecut 612702 fd 24 | 23512 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612702 fd 18 | 23512 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612702 fd 17 | 23512 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 612702 fd 9 | 23510 | gfx 33.28%, compute 16.47%, enc 507.90% | vram 1052308 KiB, gtt 256292 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183380 KiB, requested-gtt 125220 KiB |

spherecut client ids with gfx and enc/dec both rising: 23510

### B2-3

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 614245: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.81% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.81% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.81% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| spherecut 614245 fd 23 | 23607 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614245 fd 18 | 23607 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614245 fd 17 | 23607 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614245 fd 9 | 23605 | gfx 33.65%, compute 16.36%, enc 553.03% | vram 1052308 KiB, gtt 252192 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183380 KiB, requested-gtt 121120 KiB |

spherecut client ids with gfx and enc/dec both rising: 23605

### C-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 612099: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 36.08% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 36.08% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 36.08% | vram 241436 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241436 KiB, requested-gtt 12304 KiB |
| spherecut 612099 fd 9 | 23474 | gfx 205.87%, compute 124.37%, enc 740.39% | vram 1160060 KiB, gtt 367280 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1422204 KiB, requested-gtt 105136 KiB |

spherecut client ids with gfx and enc/dec both rising: 23474

### C-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 613692: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 30.63% | vram 243484 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243484 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 30.63% | vram 243484 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243484 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 30.63% | vram 243484 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243484 KiB, requested-gtt 12304 KiB |
| spherecut 613692 fd 9 | 23569 | gfx 160.22%, compute 99.91%, enc 634.57% | vram 1160052 KiB, gtt 369328 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1422196 KiB, requested-gtt 107184 KiB |

spherecut client ids with gfx and enc/dec both rising: 23569

### C2-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 611494: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.75% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.75% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.75% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 611494 fd 9 | 23437 | gfx 50.77%, compute 24.47%, enc 697.96% | vram 1160036 KiB, gtt 232208 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1291108 KiB, requested-gtt 101136 KiB |

spherecut client ids with gfx and enc/dec both rising: 23437

### C2-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 612899: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 20.45% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 20.45% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 20.46% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 612899 fd 9 | 23521 | gfx 50.74%, compute 25.23%, enc 729.58% | vram 1160036 KiB, gtt 230160 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1291108 KiB, requested-gtt 99088 KiB |

spherecut client ids with gfx and enc/dec both rising: 23521

### C2-3

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 614428: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.86% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.86% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.86% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| spherecut 614428 fd 9 | 23616 | gfx 51.62%, compute 25.33%, enc 727.80% | vram 1160044 KiB, gtt 230160 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1291116 KiB, requested-gtt 99088 KiB |

spherecut client ids with gfx and enc/dec both rising: 23616

### D-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 611657: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 20.56% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 20.56% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 20.56% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 611657 fd 24 | 23453 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611657 fd 19 | 23453 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611657 fd 17 | 23453 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611657 fd 9 | 23451 | gfx 174.39%, compute 85.85%, enc 815.55% | vram 1052304 KiB, gtt 256288 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183376 KiB, requested-gtt 125216 KiB |

spherecut client ids with gfx and enc/dec both rising: 23451

### D-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 613094: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 21.61% | vram 241444 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241444 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 21.60% | vram 241444 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241444 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 21.58% | vram 241444 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241444 KiB, requested-gtt 12304 KiB |
| spherecut 613094 fd 23 | 23537 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613094 fd 19 | 23537 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613094 fd 18 | 23537 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613094 fd 9 | 23535 | gfx 166.53%, compute 83.70%, enc 818.42% | vram 1052328 KiB, gtt 302840 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183400 KiB, requested-gtt 171768 KiB |

spherecut client ids with gfx and enc/dec both rising: 23535

### D-3

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 614620: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 21.20% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 21.20% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 21.22% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| spherecut 614620 fd 24 | 23632 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614620 fd 18 | 23632 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614620 fd 17 | 23632 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614620 fd 9 | 23630 | gfx 152.86%, compute 77.45%, enc 831.47% | vram 1052312 KiB, gtt 302828 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183384 KiB, requested-gtt 171756 KiB |

spherecut client ids with gfx and enc/dec both rising: 23630

### E-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 615431: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.49% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.49% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.49% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| spherecut 615431 fd 26 | 23675 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 615431 fd 18 | 23675 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 615431 fd 17 | 23675 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 615431 fd 9 | 23673 | gfx 61.52%, compute 30.10%, enc 757.83% | vram 1031868 KiB, gtt 256288 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1162940 KiB, requested-gtt 125216 KiB |

spherecut client ids with gfx and enc/dec both rising: 23673

### F-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 615626: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 19.59%, compute 0.00% | vram 243488 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243488 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 19.58%, compute 0.00% | vram 243488 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243488 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 19.58%, compute 0.00% | vram 243488 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243488 KiB, requested-gtt 12304 KiB |
| spherecut 615626 fd 24 | 23686 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 615626 fd 23 | 23686 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 615626 fd 19 | 23686 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 615626 fd 9 | 23684 | gfx 57.56%, compute 30.77%, enc 749.90% | vram 1056412 KiB, gtt 256284 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1187484 KiB, requested-gtt 125212 KiB |

spherecut client ids with gfx and enc/dec both rising: 23684

### L-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; ffmpeg 611826: 3 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 611889: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| ffmpeg 611826 fd 6 | 23459 | enc 138.10% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| ffmpeg 611826 fd 5 | 23459 | enc 138.10% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| ffmpeg 611826 fd 4 | 23459 | enc 138.11% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.72% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.72% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.72% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 611889 fd 24 | 23465 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611889 fd 18 | 23465 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611889 fd 17 | 23465 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 611889 fd 9 | 23463 | gfx 110.69%, compute 59.80%, enc 435.65% | vram 1056408 KiB, gtt 256264 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1187480 KiB, requested-gtt 125192 KiB |

spherecut client ids with gfx and enc/dec both rising: 23463

### L-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; ffmpeg 613248: 3 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 613317: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| ffmpeg 613248 fd 6 | 23543 | enc 263.17% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| ffmpeg 613248 fd 5 | 23543 | enc 263.17% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| ffmpeg 613248 fd 4 | 23543 | enc 263.17% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.97% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.97% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.97% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 613317 fd 26 | 23549 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613317 fd 18 | 23549 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613317 fd 17 | 23549 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613317 fd 9 | 23547 | gfx 133.64%, compute 71.36%, enc 279.65% | vram 1056408 KiB, gtt 255360 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1187480 KiB, requested-gtt 124288 KiB |

spherecut client ids with gfx and enc/dec both rising: 23547

### L-3

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; ffmpeg 614759: 3 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 614860: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| ffmpeg 614759 fd 6 | 23638 | enc 177.14% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| ffmpeg 614759 fd 5 | 23638 | enc 177.14% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| ffmpeg 614759 fd 4 | 23638 | enc 177.14% | vram 317676 KiB, gtt 9748 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 317676 KiB, requested-gtt 9748 KiB |
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.20% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.20% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.20% | vram 243500 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243500 KiB, requested-gtt 12304 KiB |
| spherecut 614860 fd 24 | 23644 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614860 fd 23 | 23644 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614860 fd 18 | 23644 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 614860 fd 9 | 23642 | gfx 128.49%, compute 67.15%, enc 377.43% | vram 1052316 KiB, gtt 256284 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183388 KiB, requested-gtt 125212 KiB |

spherecut client ids with gfx and enc/dec both rising: 23642

### N-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 610909: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 20.78% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 20.78% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 20.78% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 610909 fd 25 | 23406 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 610909 fd 19 | 23406 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 610909 fd 18 | 23406 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 610909 fd 9 | 23404 | gfx 57.48%, compute 29.72%, enc 758.88% | vram 1052316 KiB, gtt 255352 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183388 KiB, requested-gtt 124280 KiB |

spherecut client ids with gfx and enc/dec both rising: 23404

### N-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 613514: 4 fds, 2 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 20.96% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 20.96% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 20.96% | vram 241440 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 241440 KiB, requested-gtt 12304 KiB |
| spherecut 613514 fd 24 | 23560 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613514 fd 18 | 23560 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613514 fd 17 | 23560 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| spherecut 613514 fd 9 | 23558 | gfx 61.99%, compute 31.79%, enc 747.81% | vram 1052304 KiB, gtt 250144 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 1183376 KiB, requested-gtt 119072 KiB |

spherecut client ids with gfx and enc/dec both rising: 23558

### S-1

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 615026: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 17.68% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 17.68% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 17.68% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| spherecut 615026 fd 9 | 23653 | gfx 133.72% | vram 549468 KiB, gtt 228060 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 680540 KiB, requested-gtt 96988 KiB |

spherecut client ids with gfx and enc/dec both rising: none

### S-2

renderD128 fds and client ids a process (at 16 s): Telegram 143598: 3 fds, 1 ids; Xwayland 138898: 4 fds, 1 ids; bridge-gui 365897: 3 fds, 1 ids; claude 131508: 1 fds, 1 ids; claude-desktop 124290: 5 fds, 2 ids; claude-desktop 124196: 1 fds, 1 ids; cowork-linux-he 124552: 1 fds, 1 ids; firefox 4119444: 6 fds, 2 ids; gnome-character 3267612: 3 fds, 1 ids; gnome-shell 4536: 146 fds, 1 ids; gnome-software 4939: 1 fds, 1 ids; gnome-text-edit 759526: 1 fds, 1 ids; ibus-x11 138939: 3 fds, 1 ids; mullvad-gui 343259: 5 fds, 2 ids; mullvad-gui 343184: 1 fds, 1 ids; mullvad-gui 230414: 5 fds, 2 ids; mullvad-gui 230359: 1 fds, 1 ids; mutter-x11-fram 138934: 3 fds, 1 ids; nautilus 362807: 1 fds, 1 ids; ptyxis 123152: 1 fds, 1 ids; ptyxis 16025: 4 fds, 2 ids; spherecut 615228: 1 fds, 1 ids; thunderbird-bin 366487: 6 fds, 2 ids; xdg-desktop-por 5038: 1 fds, 1 ids

| process | client id | engine deltas ÷ 10 s (busy %) | drm-memory-* at 16 s | amd-* at 16 s |
|---|---|---|---|---|
| firefox 4119444 fd 73 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 71 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 68 | 20224 | none | vram 12 KiB, gtt 2048 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 12 KiB, requested-gtt 2048 KiB |
| firefox 4119444 fd 57 | 20222 | gfx 18.09% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 48 | 20222 | gfx 18.09% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| firefox 4119444 fd 23 | 20222 | gfx 18.09% | vram 243496 KiB, gtt 12304 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 243496 KiB, requested-gtt 12304 KiB |
| spherecut 615228 fd 9 | 23663 | gfx 186.14% | vram 418392 KiB, gtt 353340 KiB, cpu 0 KiB | evicted-vram 0 KiB, requested-vram 680536 KiB, requested-gtt 91196 KiB |

spherecut client ids with gfx and enc/dec both rising: none

Every key seen: amd-evicted-vram, amd-requested-gtt, amd-requested-vram, drm-client-id, drm-driver, drm-engine-compute, drm-engine-enc, drm-engine-gfx, drm-memory-cpu, drm-memory-gtt, drm-memory-vram, drm-pdev, drm-purgeable-cpu, drm-purgeable-doorbell, drm-purgeable-gds, drm-purgeable-gtt, drm-purgeable-gws, drm-purgeable-mmioremap, drm-purgeable-oa, drm-purgeable-vram, drm-resident-cpu, drm-resident-doorbell, drm-resident-gds, drm-resident-gtt, drm-resident-gws, drm-resident-mmioremap, drm-resident-oa, drm-resident-vram, drm-shared-cpu, drm-shared-doorbell, drm-shared-gds, drm-shared-gtt, drm-shared-gws, drm-shared-mmioremap, drm-shared-oa, drm-shared-vram, drm-total-cpu, drm-total-doorbell, drm-total-gds, drm-total-gtt, drm-total-gws, drm-total-mmioremap, drm-total-oa, drm-total-vram, flags, ino, mnt_id, pasid, pos

gnome-shell's 146 renderD128 fds all carry client id 22, and its fdinfo has no `drm-engine-*` keys at all, so its GPU time does not show here. Firefox's 6 fds carry 2 ids. spherecut's main client (fd 9) carries gfx, compute and enc together in one client id. The `enc` deltas go over 100 % of 10 s (757 % in A-1, 811 % in B-1); they are printed as the kernel gives them.

## 6. gpu.tsv

| run | busy mean % | sclk levels | mclk | vclk | dclk | pcie | before big gaps (sclk/mclk/vclk/dclk/pcie, 100 ms) | vram used range GB |
|---|---|---|---|---|---|---|---|---|
| A-1 | 12 | 1:100% | 0:8% 1:71% 2:11% 3:8% | 1:99% S:0% | 0:96% 1:2% S:0% | 1:100% | 1/1/1/0/1×37 1/3/1/0/1×8 1/2/1/0/1×8 1/0/1/0/1×7 | 1.96–3.09 |
| A-2 | 12 | 1:100% | 0:8% 1:71% 2:10% 3:9% | 1:99% S:0% | 0:95% 1:3% S:0% | 1:100% | 1/1/1/0/1×30 1/3/1/0/1×9 1/0/1/0/1×4 1/2/1/0/1×3 | 1.96–3.04 |
| A-3 | 12 | 1:100% | 0:10% 1:68% 2:11% 3:9% | 1:99% S:0% | 0:95% 1:3% S:0% | 1:100% | 1/1/1/0/1×26 1/2/1/0/1×7 1/3/1/0/1×2 1/0/1/0/1×1 | 1.97–3.09 |
| A-end | 12 | 1:100% | 0:8% 1:73% 2:9% 3:8% | 1:100% | 0:95% 1:4% | 1:100% | 1/1/1/0/1×25 1/2/1/0/1×8 1/0/1/0/1×5 1/3/1/0/1×2 | 1.97–3.09 |
| B-1 | 12 | 1:100% | 0:1% 1:21% 2:42% 3:33% | 1:99% S:0% | 0:32% 1:66% S:0% | 1:100% | 1/2/1/1/1×2 1/1/1/1/1×2 | 1.96–3.87 |
| B-2 | 12 | 1:100% | 0:2% 1:29% 2:29% 3:38% | 1:99% S:0% | 0:28% 1:71% S:0% | 1:100% | – | 1.96–3.88 |
| B-3 | 12 | 1:100% | 0:2% 1:28% 2:34% 3:33% | 1:99% S:0% | 0:26% 1:72% S:0% | 1:100% | 1/0/1/1/1×2 1/2/1/1/1×1 1/1/1/1/1×1 | 1.97–3.88 |
| B2-1 | 9 | 1:100% | 0:12% 1:74% 2:5% 3:6% | 1:99% S:0% | 0:91% 1:7% S:0% | 1:100% | 1/1/1/0/1×25 1/2/1/0/1×2 1/1/1/1/1×1 | 1.96–3.09 |
| B2-2 | 9 | 1:100% | 0:12% 1:74% 2:8% 3:4% | 1:99% S:0% | 0:90% 1:8% S:0% | 1:100% | 1/1/1/0/1×13 1/0/1/0/1×5 1/1/1/1/1×1 1/2/1/0/1×1 | 1.96–3.09 |
| B2-3 | 9 | 1:100% | 0:13% 1:71% 2:11% 3:3% | 1:99% S:0% | 0:93% 1:5% S:0% | 1:100% | 1/1/1/0/1×25 1/2/1/0/1×2 1/1/1/1/1×1 | 1.97–3.04 |
| C-1 | 33 | 1:100% | 0:2% 1:26% 2:37% 3:33% | 1:97% S:2% | 0:45% 1:52% S:2% | 1:100% | – | 1.96–3.16 |
| C-2 | 31 | 1:100% | 0:4% 1:27% 2:35% 3:32% | 1:96% S:3% | 0:58% 1:38% S:3% | 1:100% | – | 1.96–3.16 |
| C2-1 | 12 | 1:100% | 0:6% 1:73% 2:11% 3:9% | 1:98% S:1% | 0:76% 1:22% S:1% | 1:100% | – | 1.96–3.16 |
| C2-2 | 12 | 1:100% | 0:8% 1:67% 2:20% 3:3% | 1:98% S:1% | 0:70% 1:28% S:1% | 1:100% | – | 1.96–3.16 |
| C2-3 | 12 | 1:100% | 0:9% 1:68% 2:18% 3:3% | 1:98% S:1% | 0:71% 1:27% S:1% | 1:100% | – | 1.97–3.16 |
| D-1 | 23 | 1:100% | 0:2% 1:49% 2:25% 3:22% | 1:99% S:0% | 0:58% 1:40% S:0% | 1:100% | 1/1/1/1/1×4 1/1/1/0/1×3 1/3/1/0/1×1 | 1.96–3.09 |
| D-2 | 23 | 1:100% | 0:2% 1:49% 2:25% 3:22% | 1:99% S:0% | 0:67% 1:31% S:0% | 1:100% | 1/3/1/0/1×4 1/1/1/0/1×2 1/2/1/0/1×2 | 1.96–3.09 |
| D-3 | 23 | 1:100% | 0:3% 1:50% 2:25% 3:21% | 1:99% S:0% | 0:63% 1:35% S:0% | 1:100% | – | 1.97–3.05 |
| E-1 | 12 | 1:100% | 0:7% 1:72% 2:11% 3:9% | 1:99% S:0% | 0:95% 1:3% S:0% | 1:100% | 1/1/1/0/1×27 1/2/1/0/1×10 1/0/1/0/1×4 1/3/1/0/1×2 | 1.97–3.07 |
| F-1 | 12 | 1:100% | 0:7% 1:70% 2:11% 3:10% | 1:99% S:0% | 0:97% 1:1% S:0% | 1:100% | 1/1/1/0/1×29 1/3/1/0/1×9 1/2/1/0/1×2 1/0/1/0/1×2 | 1.97–3.09 |
| L-1 | 19 | 1:100% | 0:1% 1:60% 2:20% 3:17% | 1:100% | 0:52% 1:47% | 1:100% | – | 2.29–3.37 |
| L-2 | 20 | 1:100% | 0:1% 1:54% 2:25% 3:18% | 1:100% | 0:59% 1:40% | 1:100% | – | 2.29–3.50 |
| L-3 | 20 | 1:100% | 0:1% 1:56% 2:25% 3:16% | 1:100% | 0:44% 1:55% | 1:100% | – | 2.29–3.42 |
| S-1 | 17 | 1:100% | 0:0% 1:3% 2:1% 3:93% | 1:6% S:93% | 0:6% S:93% | 1:100% | – | 1.97–2.53 |
| S-2 | 21 | 1:100% | 1:3% 2:1% 3:95% | 1:4% S:95% | 0:4% S:95% | 1:100% | – | 1.97–2.39 |

N ran without the sampler's sysfs reads, so it has no gpu.tsv. Against A on the JSON's numbers: big gaps 0.66 and 0.92 a covered second (A 0.76 to 1.18), 40–95 ms gaps 3.17 and 4.32 a second (A 2.75 to 4.11), p99 100.0 and 100.2 ms (A 100.1 to 116.5).

## 7. LIBVA (arm F)

Trace files: libva.130651.thd-0x000964d0, libva.130651.thd-0x000964d5, libva.130712.thd-0x000964ca. Times come from the trace's own timestamps. The trace prints an entry line only for some calls; for CreateImage, DestroyImage, GetImage, MapBuffer2, UnmapBuffer there is none, so their time is from the previous traced line on that thread to their return line (calls run in order on one thread).

| call | n | p50 ms | p95 ms | max ms | timed from |
|---|---|---|---|---|---|
| vaSyncSurface | 142 | 0.01 | 0.02 | 0.03 | entry line |
| vaCreateImage | 138 | 0.01 | 0.03 | 0.04 | previous line |
| vaGetImage | 138 | 94.28 | 162.06 | 163.89 | previous line |
| vaMapBuffer2 | 142 | 0.01 | 0.01 | 0.02 | previous line |
| vaUnmapBuffer | 1601 | 0.00 | 23.33 | 28.13 | previous line |
| vaDestroyImage | 155 | 3.98 | 4.75 | 5.42 | previous line |
| vaBeginPicture | 476 | 0.01 | 0.05 | 7.60 | entry line |
| vaRenderPicture | 948 | 0.08 | 0.51 | 46.44 | entry line |
| vaEndPicture | 476 | 0.11 | 0.17 | 30.62 | entry line |

vaSyncSurface on cycles that decoded an I (CRA) picture since the previous sync: n 27, p50 0.00, p95 0.01, max 0.02 ms; on the others: n 115, p50 0.01, p95 0.02, max 0.03 ms.

vaEndPicture calls between successive vaSyncSurface calls: 0: 3×, 1: 13×, 2: 4×, 3: 52×, 4: 52×, 5: 18×

## 8. strace (arm G)

76027 completed calls; main thread 615852; run time 0 at the smoke-run line.

DRM ioctls on renderD128, per thread and request: total, then counts in run seconds 0 to 20:

- **spherecut 615852**
  - SYNCOBJ_TRANSFER: 5421: 385 288 234 246 240 240 246 258 234 252 228 252 270 252 296 300 294 274 300 272 60
  - SYNCOBJ_TIMELINE_WAIT: 4514: 319 240 195 205 200 200 205 215 195 210 190 210 225 210 247 251 244 228 250 227 48
  - SYNCOBJ_DESTROY: 3674: 271 192 156 164 160 160 164 172 156 168 152 168 180 168 198 200 196 182 200 180 87
  - SYNCOBJ_CREATE: 3646: 288 192 156 164 160 160 164 172 156 168 152 168 180 168 199 200 196 181 200 182 40
  - SYNCOBJ_RESET: 3612: 256 192 156 164 160 160 164 172 156 168 152 168 180 168 196 200 196 184 200 180 40
  - SYNCOBJ_HANDLE_TO_FD: 1814: 136 96 78 82 80 80 82 86 78 84 76 84 90 84 100 100 98 90 100 92 18
  - AMDGPU_CS: 1808: 130 96 78 82 80 80 82 86 78 84 76 84 90 84 98 101 97 92 100 90 20
  - SYNCOBJ_QUERY: 1807: 129 96 78 82 80 80 82 86 78 84 76 84 90 84 99 100 98 91 100 91 19
  - SYNCOBJ_FD_TO_HANDLE: 913: 74 48 39 41 40 40 41 43 39 42 38 42 45 42 50 50 49 45 50 45 10
  - SYNCOBJ_WAIT: 905: 64 48 39 41 40 40 41 43 39 42 38 42 45 42 49 50 49 46 50 45 12
  - AMDGPU_GEM_VA: 732: 201 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 531
  - GEM_CLOSE: 539: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 534
  - AMDGPU_GEM_CREATE: 196: 196 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_MMAP: 187: 187 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_INFO: 41: 41 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_CTX: 11: 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4
  - VERSION: 8: 8 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_METADATA: 6: 6 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - PRIME_HANDLE_TO_FD: 6: 6 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - GET_CAP: 5: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_SIGNAL: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- **decode-v0 615875**
  - AMDGPU_GEM_MMAP: 1106: 77 59 53 53 53 53 52 55 50 53 52 52 56 51 56 58 59 51 58 53 2
  - SYNCOBJ_CREATE: 740: 55 37 37 35 35 35 35 35 37 35 35 35 37 36 37 38 37 33 38 34 4
  - SYNCOBJ_WAIT: 737: 51 36 37 35 35 35 35 36 36 35 35 36 36 37 37 38 37 33 38 34 5
  - SYNCOBJ_DESTROY: 499: 41 27 25 24 24 23 24 23 27 23 24 21 25 25 24 24 22 20 25 20 8
  - AMDGPU_GEM_VA: 313: 58 14 15 13 14 14 13 10 16 13 13 12 12 14 12 12 10 10 12 10 16
  - AMDGPU_GEM_CREATE: 162: 40 8 7 7 7 7 6 5 8 7 6 6 6 7 6 6 5 5 6 5 2
  - GEM_CLOSE: 152: 19 6 8 6 7 7 7 5 8 6 7 6 6 7 6 6 5 5 6 5 14
  - AMDGPU_INFO: 31: 31 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - VERSION: 12: 12 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - GET_CAP: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_CTX: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- **upload 615869**
  - AMDGPU_GEM_VA: 322: 247 37 0 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1
  - AMDGPU_GEM_CREATE: 321: 247 37 0 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_MMAP: 320: 246 37 0 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_QUERY: 259: 26 12 14 12 12 12 12 12 14 12 12 12 12 14 12 12 12 8 12 10 5
  - SYNCOBJ_TIMELINE_WAIT: 128: 12 6 7 6 6 6 6 6 7 6 6 6 6 7 6 6 6 4 6 5 2
  - AMDGPU_CS: 128: 12 6 7 6 6 6 6 6 7 6 6 6 6 7 6 6 6 4 6 5 2
  - SYNCOBJ_RESET: 128: 12 6 7 6 6 6 6 6 7 6 6 6 6 7 6 6 6 4 6 5 2
  - GEM_CLOSE: 1: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1
- **doctor 615870**
  - AMDGPU_GEM_VA: 430: 430 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_CREATE: 218: 218 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - GEM_CLOSE: 210: 210 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_MMAP: 195: 195 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_CREATE: 188: 188 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_DESTROY: 178: 178 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_WAIT: 44: 44 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_INFO: 31: 31 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_WAIT_IDLE: 19: 19 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - VERSION: 16: 16 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_TIMELINE_WAIT: 16: 16 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_RESET: 12: 12 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_CS: 12: 12 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_CTX: 10: 10 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - GET_CAP: 7: 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_USERPTR: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- **spherecut:cs0 615880**
  - AMDGPU_CS: 777: 92 37 37 35 35 35 35 35 37 35 35 35 37 36 37 38 37 33 38 34 4
  - SYNCOBJ_DESTROY: 240: 16 11 10 11 11 11 11 13 8 12 11 13 12 11 13 14 13 14 13 12 0
  - AMDGPU_GEM_VA: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - GEM_CLOSE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- **spherecut:sh0 615882**
  - AMDGPU_GEM_CREATE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_VA: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_MMAP: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_CREATE: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_CTX: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- **spherecut:sh0 615886**
  - SYNCOBJ_CREATE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_CTX: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_CREATE: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - AMDGPU_GEM_MMAP: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  - SYNCOBJ_DESTROY: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0

mmap and munmap of 32 MiB or more, per thread, counts in run seconds 0 to 20:

- spherecut 615852 mmap: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 615853 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 615854 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 615865 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- smithay-clipboa 615866 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 615869 mmap: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615870 mmap: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- files 615871 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- probe 615872 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- open 615873 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- smoke-watchdog 615874 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- decode-v0 615875 mmap: 388: 40 18 21 18 18 18 18 16 23 18 18 18 18 21 18 18 15 15 18 15 6
- decode-v0 615882 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- decode-v0 615883 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- decode-v0 615884 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615885 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615886 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615887 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615888 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- untitled projec 615889 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615891 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 615892 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 615893 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- rpm 615894 mmap: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0

Main-thread syscalls of 20 ms or more (run s, call, fd target, request, ms):

- 0.771 epoll_pwait anon_inode:[eventpoll]  42.5
- 0.921 epoll_pwait anon_inode:[eventpoll]  89.7
- 1.171 epoll_pwait anon_inode:[eventpoll]  39.2
- 1.304 epoll_pwait anon_inode:[eventpoll]  65.7
- 1.688 epoll_pwait anon_inode:[eventpoll]  30.6
- 1.954 epoll_pwait anon_inode:[eventpoll]  70.7
- 2.138 epoll_pwait anon_inode:[eventpoll]  49.9
- 2.321 epoll_pwait anon_inode:[eventpoll]  27.4
- 2.437 epoll_pwait anon_inode:[eventpoll]  44.7
- 2.571 epoll_pwait anon_inode:[eventpoll]  44.2
- 2.704 epoll_pwait anon_inode:[eventpoll]  44.4
- 2.838 epoll_pwait anon_inode:[eventpoll]  43.0
- 2.971 epoll_pwait anon_inode:[eventpoll]  107.5
- 3.171 epoll_pwait anon_inode:[eventpoll]  71.5
- 3.554 epoll_pwait anon_inode:[eventpoll]  46.0
- 3.688 epoll_pwait anon_inode:[eventpoll]  71.0
- 3.854 epoll_pwait anon_inode:[eventpoll]  36.4
- 3.971 epoll_pwait anon_inode:[eventpoll]  118.0
- 4.171 epoll_pwait anon_inode:[eventpoll]  80.8
- 4.554 epoll_pwait anon_inode:[eventpoll]  42.7
- 4.688 epoll_pwait anon_inode:[eventpoll]  61.0
- 4.838 epoll_pwait anon_inode:[eventpoll]  40.9
- 4.971 epoll_pwait anon_inode:[eventpoll]  106.8
- 5.171 epoll_pwait anon_inode:[eventpoll]  70.1
- 5.554 epoll_pwait anon_inode:[eventpoll]  43.5
- 5.688 epoll_pwait anon_inode:[eventpoll]  69.1
- 5.838 epoll_pwait anon_inode:[eventpoll]  49.5
- 5.971 epoll_pwait anon_inode:[eventpoll]  113.1
- 6.171 epoll_pwait anon_inode:[eventpoll]  73.2
- 6.554 epoll_pwait anon_inode:[eventpoll]  47.9
- 6.688 epoll_pwait anon_inode:[eventpoll]  73.4
- 6.854 epoll_pwait anon_inode:[eventpoll]  37.6
- 6.988 epoll_pwait anon_inode:[eventpoll]  103.0
- 7.171 epoll_pwait anon_inode:[eventpoll]  81.8
- 7.571 epoll_pwait anon_inode:[eventpoll]  38.6
- 7.688 epoll_pwait anon_inode:[eventpoll]  78.9
- 8.071 epoll_pwait anon_inode:[eventpoll]  22.5
- 8.171 epoll_pwait anon_inode:[eventpoll]  56.8
- 8.321 epoll_pwait anon_inode:[eventpoll]  38.9
- 8.438 epoll_pwait anon_inode:[eventpoll]  54.3
- 8.571 epoll_pwait anon_inode:[eventpoll]  52.3
- 8.704 epoll_pwait anon_inode:[eventpoll]  49.7
- 8.838 epoll_pwait anon_inode:[eventpoll]  45.5
- 8.971 epoll_pwait anon_inode:[eventpoll]  109.8
- 9.171 epoll_pwait anon_inode:[eventpoll]  71.9
- 9.554 epoll_pwait anon_inode:[eventpoll]  40.8
- 9.721 epoll_pwait anon_inode:[eventpoll]  32.5
- 9.854 epoll_pwait anon_inode:[eventpoll]  28.9
- 9.971 epoll_pwait anon_inode:[eventpoll]  114.1
- 10.171 epoll_pwait anon_inode:[eventpoll]  76.7
- 10.554 epoll_pwait anon_inode:[eventpoll]  50.7
- 10.688 epoll_pwait anon_inode:[eventpoll]  75.8
- 10.854 epoll_pwait anon_inode:[eventpoll]  29.5
- 10.971 epoll_pwait anon_inode:[eventpoll]  109.4
- 11.171 epoll_pwait anon_inode:[eventpoll]  72.4
- 11.554 epoll_pwait anon_inode:[eventpoll]  35.0
- 11.821 epoll_pwait anon_inode:[eventpoll]  33.9
- 11.954 epoll_pwait anon_inode:[eventpoll]  69.2
- 12.188 epoll_pwait anon_inode:[eventpoll]  41.7
- 12.321 epoll_pwait anon_inode:[eventpoll]  70.8
- 12.704 epoll_pwait anon_inode:[eventpoll]  38.0
- 12.838 epoll_pwait anon_inode:[eventpoll]  50.6
- 12.971 epoll_pwait anon_inode:[eventpoll]  72.3
- 13.288 epoll_pwait anon_inode:[eventpoll]  51.5
- 13.421 epoll_pwait anon_inode:[eventpoll]  52.0
- 13.554 epoll_pwait anon_inode:[eventpoll]  41.2
- 13.938 epoll_pwait anon_inode:[eventpoll]  68.7
- 14.171 epoll_pwait anon_inode:[eventpoll]  45.8
- 14.304 epoll_pwait anon_inode:[eventpoll]  75.1
- 14.688 epoll_pwait anon_inode:[eventpoll]  30.2
- 15.304 epoll_pwait anon_inode:[eventpoll]  57.1
- 15.454 epoll_pwait anon_inode:[eventpoll]  62.0
- 15.821 epoll_pwait anon_inode:[eventpoll]  37.8
- 16.004 epoll_pwait anon_inode:[eventpoll]  56.3
- 16.438 epoll_pwait anon_inode:[eventpoll]  44.2
- 16.571 epoll_pwait anon_inode:[eventpoll]  64.3
- 17.121 epoll_pwait anon_inode:[eventpoll]  82.3
- 17.371 epoll_pwait anon_inode:[eventpoll]  86.3
- 17.804 epoll_pwait anon_inode:[eventpoll]  47.2
- 18.304 epoll_pwait anon_inode:[eventpoll]  52.3
- 18.438 epoll_pwait anon_inode:[eventpoll]  73.9
- 18.804 epoll_pwait anon_inode:[eventpoll]  51.9
- 18.988 epoll_pwait anon_inode:[eventpoll]  66.6
- 19.254 epoll_pwait anon_inode:[eventpoll]  37.7
- 19.438 epoll_pwait anon_inode:[eventpoll]  48.9
- 19.571 epoll_pwait anon_inode:[eventpoll]  70.6
- 19.938 epoll_pwait anon_inode:[eventpoll]  40.2

Main-thread waits of 40 ms or more: 70. GEM_CLOSE or GEM_VA on any thread in the 200 ms before one:

- wait at 0.771 (43 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×9, decode-v0 GEM_CLOSE ×7, upload AMDGPU_GEM_VA ×37
- wait at 0.921 (90 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×2
- wait at 1.304 (66 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 1.954 (71 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×2
- wait at 2.138 (50 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×3, decode-v0 GEM_CLOSE ×2
- wait at 2.437 (45 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 2.571 (44 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 2.704 (44 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 2.838 (43 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 2.971 (108 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 3.171 (72 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 3.554 (46 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2, upload AMDGPU_GEM_VA ×37
- wait at 3.688 (71 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 3.971 (118 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×2
- wait at 4.171 (81 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 4.554 (43 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 4.688 (61 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 4.838 (41 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 4.971 (107 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×2
- wait at 5.171 (70 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 5.554 (44 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 5.688 (69 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 5.838 (50 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 5.971 (113 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×2
- wait at 6.171 (73 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 6.554 (48 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 6.688 (73 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 6.988 (103 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 7.171 (82 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 7.688 (79 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 8.171 (57 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 8.438 (54 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 8.571 (52 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 8.704 (50 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 8.838 (46 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 8.971 (110 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 9.171 (72 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 9.554 (41 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 9.971 (114 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×2
- wait at 10.171 (77 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 10.554 (51 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 10.688 (76 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×5, decode-v0 GEM_CLOSE ×3
- wait at 10.971 (109 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 11.171 (72 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 11.954 (69 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 12.188 (42 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 12.321 (71 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 12.838 (51 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 12.971 (72 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 13.288 (51 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 13.421 (52 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 13.554 (41 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 13.938 (69 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 14.171 (46 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 14.304 (75 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 15.304 (57 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 15.454 (62 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 16.004 (56 ms, epoll_pwait): decode-v0 GEM_CLOSE ×2, decode-v0 AMDGPU_GEM_VA ×2
- wait at 16.438 (44 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×3, decode-v0 GEM_CLOSE ×2
- wait at 16.571 (64 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 17.121 (82 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×3, decode-v0 GEM_CLOSE ×2
- wait at 17.371 (86 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×2, decode-v0 GEM_CLOSE ×1
- wait at 17.804 (47 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 18.304 (52 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 18.438 (74 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 18.804 (52 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 18.988 (67 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×3, decode-v0 GEM_CLOSE ×2
- wait at 19.438 (49 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×3, decode-v0 GEM_CLOSE ×2
- wait at 19.571 (71 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2
- wait at 19.938 (40 ms, epoll_pwait): decode-v0 AMDGPU_GEM_VA ×4, decode-v0 GEM_CLOSE ×2

70 of 70 waits have one. GEM_CLOSE and GEM_VA over the run: spherecut AMDGPU_GEM_VA 732, spherecut GEM_CLOSE 539, doctor AMDGPU_GEM_VA 430, upload AMDGPU_GEM_VA 322, decode-v0 AMDGPU_GEM_VA 313, doctor GEM_CLOSE 210, decode-v0 GEM_CLOSE 152, spherecut:cs0 AMDGPU_GEM_VA 4, spherecut:cs0 GEM_CLOSE 4, spherecut:sh0 AMDGPU_GEM_VA 3, upload GEM_CLOSE 1

openat of /proc/self/stat or /proc/self/status, per run second: 0s stat ×2, 0s status ×4, 1s stat ×1, 1s status ×1, 2s stat ×1, 2s status ×1, 3s stat ×1, 3s status ×1, 4s stat ×1, 4s status ×1, 5s stat ×1, 5s status ×1, 6s stat ×1, 6s status ×1, 7s stat ×1, 7s status ×1, 8s stat ×1, 8s status ×1, 9s stat ×1, 9s status ×1, 10s stat ×1, 10s status ×1, 11s stat ×1, 11s status ×1, 12s stat ×1, 12s status ×1, 13s stat ×1, 13s status ×1, 14s stat ×1, 14s status ×1, 15s stat ×1, 15s status ×1, 16s stat ×1, 16s status ×1, 17s stat ×1, 17s status ×1, 18s stat ×1, 18s status ×1, 19s stat ×1, 19s status ×1

Arm G ran under strace, which slows every syscall, so its gui.json is not compared with the others.

## 9. Headless (arm H)

Timeline `hevc10-5k7-30fps-noisy-20s-va-api-realtime.csv`: 1200 ticks; tick interval p50 16.67 ms, p99 16.69, max 17.53.

Ticks more than 25 ms after the previous one (missed ticks): 0

download_max_ms over the ticks that finished a download: n 131, p50 146.0, p95 190.9.

The report's line: hevc10-5k7-30fps-noisy-20s, VA-API: 20 s in real time, 1 loops, 0 UI ticks missed; stream 0 on VA-API; stage p50/p95 ms: decode 0.96/37.93, download 146.04/190.95, upload_cpu 5.92/7.03, upload_gpu 1.89/5.62, render_cpu 0.36/0.74, render_gpu 0.32/0.56, ui_frame 16.67/16.68, shown_interval 150.00/200.01, av_offset 200.07/316.73

## 10. Verdict lines

| arm | kept samples | passes ≥ 95 ms | covered s | 40–95 ms a second | p99 ms (each) | big gaps | regular gaps |
|---|---|---|---|---|---|---|---|
| A | 3 | 36 | 35.8 | 3.55 | 116, 116, 100 | big gaps stay | regular gaps stay |
| A-end | 1 | 10 | 13.6 | 3.08 | 100 | big gaps stay (by the rule: 13.6 covered s, under 30) | regular gaps stay |
| N | 2 | 19 | 24.4 | 3.69 | 100, 100 | big gaps stay (by the rule: 24.4 covered s, under 30) | regular gaps stay |
| B | 3 | 2 | 18.2 | 3.62 | 83, 83, 83 | big gaps stay (by the rule: 18.2 covered s, under 30) | regular gaps stay |
| B2 | 3 | 19 | 56.9 | 0.98 | 67, 83, 83 | big gaps stay | regular gaps gone |
| C | 2 | 0 | 22.6 | 0.84 | 66, 66 | big gaps stay (by the rule: 22.6 covered s, under 30) | regular gaps gone |
| C2 | 3 | 0 | 57.0 | 0.88 | 34, 66, 66 | big gaps gone | regular gaps gone |
| D | 3 | 4 | 33.2 | 4.18 | 84, 85, 84 | big gaps stay | regular gaps stay |
| L | 3 | 0 | 57.0 | 0.12 | 33, 19, 33 | big gaps gone | regular gaps gone |
| S | 2 | 0 | 38.0 | 0.03 | 20, 19 | big gaps gone | regular gaps gone |
| E | 1 | 11 | 11.4 | 4.12 | 100 | big gaps stay (by the rule: 11.4 covered s, under 30) | regular gaps stay |
| F | 1 | 11 | 11.2 | 3.92 | 117 | big gaps stay (by the rule: 11.2 covered s, under 30) | regular gaps stay |

**Drift:** A-end, the last run of the trace, had 10 passes of 95 ms or more over 13.6 covered s (0.73 a second) and 3.08 passes of 40 to 95 ms a second, with p99 100.0 ms. A-1 to A-3 had 1.18, 1.07 and 0.76 a second, 2.75 to 4.11 a second, and p99 116.5, 116.4 and 100.1 ms.

**Anything odd:**

- **The main thread's waits:** almost every main-thread stretch of 25 ms or more, in every sampler run, is `epoll_pwait` on its `anon_inode:[eventpoll]` (wchan `ep_poll`). The exceptions:
  - futex stretches of 28-74 ms, 1 or 2 a run: A-3, C-1, C-2, C2-1 to C2-3, D-1, D-2, L-1, N-1, S-1 and S-2;
  - `clock_nanosleep` (230, wchan `hrtimer_nanosleep`) in E-1 (70 ms) and S-1/S-2 (128 and 138 ms);
  - in C-2 only, one mprotect stretch (64 ms) and **one AMDGPU_CS stretch of 56 ms** (wchan `amdgpu_cs_submit`).

  Over all JSON gaps of 95 ms or more, none has 50 ms or more inside AMDGPU_CS (section 3). In every sampler run, the gnome-shell thread most often seen off its usual wait while spherecut's main thread waits 40 ms or more is gnome-shell's main thread (4536), in state R.
- **libva:** vaSyncSurface returns in 0.01 ms. The wait for the decode is inside vaGetImage, at p50 94 ms and max 164 ms (section 7).
- **mclk:** in A, mclk sits at level 1 (456 MHz) 68-73 % of the time. In the runs without big gaps it sits at level 3 (1124 MHz) more of the time: S 93-95 %, B 33-38 %, C 32-33 %, L 16-18 % (section 6).
- **Download times:** C2-1's download p50 was 37 ms where C2-2 and C2-3 had 11.5 ms, and C2-1 alone had 2363 compaction stalls. The C runs show compaction and THP fallbacks; no VA-API run does.
- **L's ffmpeg:** it ran each time with `-re` on the 1920 clip, with no output on stderr.
