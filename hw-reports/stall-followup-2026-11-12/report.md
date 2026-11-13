# odin follow-up for the VA-API copy-path stall (measure only, prototype branch)

## 1. Header

- **Branch and code:** `proto/vaapi-drain` at **6675318**. The tree was clean before checkout, with no untracked files. `target/release/spherecut` was built by step 1's first hw-test (2026-11-12 16:28:02 PST), and no `.rs` file under crates or xtask is newer.
- **env-before.txt:** only `WAYLAND_DISPLAY=wayland-0` and `XDG_RUNTIME_DIR=/run/user/1000`.
- **Machine:** as in PR #36 (see env.txt).
  - Kernel 7.2.9-200.fc44; THP madvise/madvise; **ptrace_scope 0**; dpm auto.
  - PCIe 16.0 GT/s x8 at all three levels.
  - "strace options ok"; ffmpeg has vaapi.
- **Graphics stack:**
  - mutter-50.5-1.fc44, gnome-shell-50.5-1.fc44 (GNOME Shell 50.5).
  - **`wayland-info` is not installed**, so that line printed nothing. The protocols bound by spherecut are in section 5's WAYLAND_DEBUG part.
  - libva in the binary: `/usr/lib64/libva.so.2.2300.0`.
- **Other apps:** Firefox was open at the start (16:26 PST), with its main process and a content process at about 30 % CPU each and no audio stream. It was closed before the first arm run: A-1's fdinfo at 6 s shows no Firefox renderD128 fds, and only its `--dbus-service` helper remains. Nothing else was heavy.
- **Session:** the idle inhibitor (`gnome-session-inhibit … "Spherecut stall follow-up" sleep 7200`) ran throughout. No lock at any check.
- **Present mode:** Fifo in every run.
- **Runs:** none was retried or dropped; all 22 exited 0 (the 19 GUI runs and the 3 headless benches).
- **Every drain arm counted:**
  - `result.vaapi_drain` matched the mode asked.
  - `drain.unavailable` was 0, with drain samples above 0.
  - errors 0 and overflow 0 in every run.
- **Other count rules:**
  - B and Bp: cache.bytes 0, no export refused.
  - P-A, P-Ad and W-A: `result.probe.n` 4988-5003.
  - H-download and H-picture name `drain`; H-off doesn't.
- **stderr:** no "vaSyncSurface not found", "ignoring SPHERECUT_…" or "the gate probe didn't start" line in any run.

### Step 1: the official test

- **hwtest-off** (exit 1, `gui_decode_behind` failed):
  - run 0 presented 0.998, p99 17.0, shown 200, pass.
  - **run 1 failed:** "75% of the UI frames expected were presented … each pass's own work 1.5 ms at p95" and "UI p99 110.3 ms, over 1.5 × the 60 Hz period (25.0 ms)"; shown 123.
  - run 2 (control) presented 0.336, failed as it must.
- **hwtest-download** (exit 0, `gui_decode_behind` **passed**):
  - run 0 presented 1.000, p99 17.0, shown 200.
  - **run 1 presented 0.995, p99 18.4, shown 101, `failed: []`.**
  - run 2 (control) presented 0.336.
  - `inconclusive: []`.
- **Step 1's checks on base-download/gui-behind-1.json:** `vaapi_drain` "download", `drain.unavailable` 0, `stages.drain.samples` 101. hwtest-download.md's run 1 paragraph reads: "The VA-API drain (`download`): 101 frames drained, p50 / p95 / max 107.6 / 155.7 / 178.8 ms each, waiting for p50 2 and at most 3 surfaces not yet ready; 0 failed syncs, 0 surfaces forgotten past the 64 a decoder keeps." Runs 0 and 2 read "no frame drained".

## 3. One row per run

covered s, big (≥ 95 ms) and mid (40–95 ms) a covered second from `result.late`, as last time. Drain columns from `result.vaapi_drain`, `result.stages.drain`, `result.drain`. Download p50 is `result.stages.download`, now the copy alone. GTT steps: up-steps of gtt_used by 47.5 MiB ± 1 MiB between vram.tsv's 100 ms rows (sampler runs only).

| run | exit | vaapi_drain | presented | p50 / p99 ms | covered s | big/s | mid/s | drain n / p50 / p95 ms | waited/download p50 / max | errors / overflow / unavailable | download p50 / p95 | decode p50 / p95 | upload_cpu / gpu p50 | shown | decode_dropped | gate waits | GTT +47.5 steps | dropped |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A-1 | 0 | off | 0.795 | 16.7 / 92.9 | 12.8 | 0.39 | 3.51 | 0 / – / – | – / – | – / – / – | 150.3 / 193.1 | 0.9 / 38.9 | 6.0 / 2.4 | 125 | 473 | 11 | 30 |  |
| A-2 | 0 | off | 0.783 | 16.7 / 100.7 | 12.6 | 0.56 | 3.02 | 0 / – / – | – / – | – / – / – | 150.7 / 192.4 | 1.0 / 45.5 | 6.0 / 2.4 | 124 | 473 | 12 | 25 |  |
| A-3 | 0 | off | 0.760 | 16.7 / 103.2 | 12.1 | 0.49 | 3.05 | 0 / – / – | – / – | – / – / – | 155.5 / 194.1 | 1.0 / 42.6 | 6.0 / 2.4 | 119 | 479 | 9 | 32 |  |
| A-end | 0 | off | 0.769 | 16.7 / 114.9 | 10.2 | 0.39 | 4.21 | 0 / – / – | – / – | – / – / – | 151.0 / 191.1 | 1.0 / 38.1 | 6.0 / 2.4 | 130 | 467 | 17 | 36 |  |
| Ad-1 | 0 | download | 0.996 | 16.7 / 18.4 | 19.0 | 0.00 | 0.00 | 114 / 76.4 / 139.9 | 2 / 4 | 0 / 0 / 0 | 68.4 / 70.1 | 1.8 / 40.5 | 5.9 / 1.4 | 113 | 482 | 15 | 27 |  |
| Ad-2 | 0 | download | 0.997 | 16.7 / 18.0 | 19.0 | 0.00 | 0.00 | 109 / 87.0 / 153.3 | 2 / 4 | 0 / 0 / 0 | 68.8 / 70.5 | 1.6 / 38.0 | 5.8 / 1.4 | 109 | 485 | 8 | 26 |  |
| Ad-3 | 0 | download | 0.996 | 16.7 / 18.4 | 19.0 | 0.00 | 0.00 | 109 / 92.3 / 154.6 | 2 / 4 | 0 / 0 / 0 | 68.4 / 69.8 | 2.2 / 41.9 | 5.8 / 1.4 | 109 | 478 | 11 | 21 |  |
| Ap-1 | 0 | picture | 0.994 | 16.7 / 18.0 | 19.0 | 0.00 | 0.00 | 85 / 181.0 / 251.5 | 6 / 8 | 0 / 0 / 0 | 68.9 / 71.2 | 0.6 / 1.2 | 5.2 / 1.4 | 85 | 502 | 10 | 25 |  |
| Ap-2 | 0 | picture | 0.993 | 16.7 / 18.2 | 19.0 | 0.00 | 0.00 | 81 / 183.8 / 255.4 | 6 / 8 | 0 / 0 / 0 | 68.7 / 70.7 | 0.5 / 1.3 | 5.1 / 1.3 | 81 | 509 | 9 | 23 |  |
| Ap-3 | 0 | picture | 0.996 | 16.7 / 17.7 | 19.0 | 0.00 | 0.00 | 82 / 183.7 / 256.3 | 6 / 8 | 0 / 0 / 0 | 69.1 / 70.9 | 0.6 / 1.3 | 5.1 / 1.3 | 82 | 512 | 4 | 26 |  |
| B-1 | 0 | off | 0.845 | 16.7 / 64.2 | 7.5 | 0.00 | 1.73 | 0 / – / – | – / – | – / – / – | 0.0 / 4.8 | 0.8 / 1.4 | 0.4 / 1.9 | 579 | 0 | 34 | 0 |  |
| B-2 | 0 | off | 0.819 | 16.8 / 61.3 | 6.5 | 0.00 | 2.01 | 0 / – / – | – / – | – / – / – | 0.0 / 8.0 | 0.8 / 1.3 | 0.4 / 1.8 | 580 | 0 | 31 | 0 |  |
| Bp-1 | 0 | picture | 0.844 | 16.8 / 62.7 | 7.6 | 0.00 | 1.32 | 584 / 23.0 / 42.9 | 1 / 1 | 0 / 0 / 0 | 0.0 / 0.0 | 0.8 / 1.4 | 0.4 / 1.8 | 581 | 0 | 26 | 0 |  |
| Bp-2 | 0 | picture | 0.826 | 16.8 / 57.1 | 6.1 | 0.00 | 1.15 | 590 / 23.8 / 41.2 | 1 / 1 | 0 / 0 / 0 | 0.0 / 0.0 | 0.8 / 1.6 | 0.4 / 1.9 | 584 | 0 | 28 | 0 |  |
| G-A | 0 | off | 0.779 | 16.7 / 99.6 | 13.2 | 0.68 | 2.64 | 0 / – / – | – / – | – / – / – | 155.0 / 197.9 | 1.2 / 39.6 | 6.1 / 2.2 | 116 | 481 | 19 | – |  |
| G-Ad | 0 | download | 0.997 | 16.7 / 18.1 | 19.0 | 0.00 | 0.00 | 100 / 108.2 / 127.9 | 2 / 4 | 0 / 0 / 0 | 68.7 / 70.8 | 24.1 / 48.1 | 6.1 / 1.4 | 100 | 487 | 8 | – |  |
| P-A | 0 | off | 0.785 | 16.7 / 93.2 | 12.1 | 0.49 | 3.62 | 0 / – / – | – / – | – / – / – | 154.6 / 194.2 | 1.0 / 39.6 | 6.0 / 2.4 | 120 | 478 | 499 | 29 |  |
| P-Ad | 0 | download | 0.993 | 16.7 / 18.3 | 19.0 | 0.00 | 0.00 | 100 / 103.7 / 127.2 | 2 / 3 | 0 / 0 / 0 | 68.8 / 70.3 | 24.2 / 44.4 | 5.8 / 1.4 | 100 | 495 | 637 | 30 |  |
| W-A | 0 | off | 0.770 | 16.7 / 91.6 | 10.6 | 0.47 | 3.96 | 0 / – / – | – / – | – / – / – | 149.6 / 193.1 | 0.9 / 39.3 | 6.2 / 2.4 | 129 | 467 | 479 | – |  |

/proc/vmstat deltas and PSI after each run:

| run | pgfault | pgmajfault | compact_stall | thp_fault_alloc | thp_fault_fallback | pgscan_direct | pgmigrate_success | PSI mem / cpu some avg10 |
|---|---|---|---|---|---|---|---|---|
| A-1 | 3251490 | 5 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| A-2 | 3233795 | 1 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| A-3 | 3092302 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| A-end | 3339531 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.02 |
| Ad-1 | 3019749 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| Ad-2 | 2837795 | 1 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| Ad-3 | 2864985 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| Ap-1 | 2317634 | 1 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.04 |
| Ap-2 | 2151415 | 3 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| Ap-3 | 2168415 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| B-1 | 81714 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| B-2 | 82492 | 1 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| Bp-1 | 90243 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| Bp-2 | 223947 | 101 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.02 |
| G-A | 3029050 | 1 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| G-Ad | 2627983 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| P-A | 3119695 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| P-Ad | 2589164 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |
| W-A | 3331046 | 0 | 0 | 0 | 0 | 0 | 0 | 0.0 / 0.0 |

Present modes: A-1 Fifo, A-2 Fifo, A-3 Fifo, A-end Fifo, Ad-1 Fifo, Ad-2 Fifo, Ad-3 Fifo, Ap-1 Fifo, Ap-2 Fifo, Ap-3 Fifo, B-1 Fifo, B-2 Fifo, Bp-1 Fifo, Bp-2 Fifo, G-A Fifo, G-Ad Fifo, P-A Fifo, P-Ad Fifo, W-A Fifo

## 4. Coupling

### A-1

decode-v0 stretches in DRM_SYNCOBJ_WAIT / SYNCOBJ_TIMELINE_WAIT of 20 ms or more: 160, p50 64.0 ms, p95 106.0 ms.

JSON gaps of 40 ms or more: 50; share ending −2 to +20 ms after such a stretch's end: 100.0 %. Control, 300 circular shifts: p50 18.0 %, p99 34.0 %.

Main-thread waits of 40 ms or more, by syscall: epoll_pwait on anon_inode:[eventpoll]: 146 (9922 ms)

### Ad-1

decode-v0 stretches in DRM_SYNCOBJ_WAIT / SYNCOBJ_TIMELINE_WAIT of 20 ms or more: 113, p50 80.0 ms, p95 186.0 ms. The drain's vaSyncSurface calls (download) run on decode-v0 too, so its waits are among these stretches.

JSON gaps of 40 ms or more: 0; share ending −2 to +20 ms after such a stretch's end: – %. Control, 300 circular shifts: p50 – %, p99 – %.

Main-thread waits of 40 ms or more, by syscall: epoll_pwait on anon_inode:[eventpoll]: 188 (9619 ms)

### Ap-1

decode-v0 stretches in DRM_SYNCOBJ_WAIT / SYNCOBJ_TIMELINE_WAIT of 20 ms or more: 232, p50 48.0 ms, p95 120.0 ms. The drain's vaSyncSurface calls (picture) run on decode-v0 too, so its waits are among these stretches.

JSON gaps of 40 ms or more: 0; share ending −2 to +20 ms after such a stretch's end: – %. Control, 300 circular shifts: p50 – %, p99 – %.

Main-thread waits of 40 ms or more, by syscall: epoll_pwait on anon_inode:[eventpoll]: 154 (7948 ms)

### P-A

decode-v0 stretches in DRM_SYNCOBJ_WAIT / SYNCOBJ_TIMELINE_WAIT of 20 ms or more: 165, p50 64.0 ms, p95 116.0 ms.

JSON gaps of 40 ms or more: 50; share ending −2 to +20 ms after such a stretch's end: 100.0 %. Control, 300 circular shifts: p50 18.0 %, p99 38.0 %.

Main-thread waits of 40 ms or more, by syscall: epoll_pwait on anon_inode:[eventpoll]: 157 (11136 ms)

### P-Ad

decode-v0 stretches in DRM_SYNCOBJ_WAIT / SYNCOBJ_TIMELINE_WAIT of 20 ms or more: 114, p50 110.0 ms, p95 212.0 ms. The drain's vaSyncSurface calls (download) run on decode-v0 too, so its waits are among these stretches.

JSON gaps of 40 ms or more: 0; share ending −2 to +20 ms after such a stretch's end: – %. Control, 300 circular shifts: p50 – %, p99 – %.

Main-thread waits of 40 ms or more, by syscall: epoll_pwait on anon_inode:[eventpoll]: 188 (9847 ms)

## 5. Probe, Wayland, strace, headless

### Probe

**P-A**: period 4.0 ms, n 4995, p50 0.94, p99 97.59, max 149.01 ms, over_8ms listed 1419, over_8ms_dropped 0, unfinished 0. Entries of 20 ms or more: 1114, 55.67 a second; of 30 ms or more: 891, 44.53 a second (played_s 20.01).

JSON gaps of 40 ms or more: 50. Largest listed latency among jobs submitted in a gap's first 20 ms: 50 gaps have one (p50 58.0, p95 110.2, max 135.3 ms); 0 gaps: 8 ms or less.

Gap end minus the completion of the last job submitted before the gap: 39 gaps where that job is listed: p50 1.7 ms, p5 1.0, p95 2.4; 11 gaps whose last job took 8 ms or less.

**P-Ad**: period 4.0 ms, n 5003, p50 0.70, p99 4.09, max 7.08 ms, over_8ms listed 0, over_8ms_dropped 0, unfinished 0. Entries of 20 ms or more: 0, 0.00 a second; of 30 ms or more: 0, 0.00 a second (played_s 20.01).

JSON gaps of 40 ms or more: 0. Largest listed latency among jobs submitted in a gap's first 20 ms: 0 gaps have one (p50 –, p95 –, max – ms); 0 gaps: 8 ms or less.

Gap end minus the completion of the last job submitted before the gap: 0 gaps where that job is listed: p50 – ms, p5 –, p95 –; 0 gaps whose last job took 8 ms or less.

**W-A**: period 4.0 ms, n 4988, p50 1.00, p99 106.03, max 147.58 ms, over_8ms listed 1579, over_8ms_dropped 0, unfinished 0. Entries of 20 ms or more: 1223, 61.11 a second; of 30 ms or more: 960, 47.97 a second (played_s 20.01).

JSON gaps of 40 ms or more: 47. Largest listed latency among jobs submitted in a gap's first 20 ms: 47 gaps have one (p50 54.5, p95 123.0, max 143.4 ms); 0 gaps: 8 ms or less.

Gap end minus the completion of the last job submitted before the gap: 41 gaps where that job is listed: p50 1.8 ms, p5 1.2, p95 2.7; 6 gaps whose last job took 8 ms or less.

### WAYLAND_DEBUG (W-A)

19335 Wayland lines parsed.

Bound globals (bind requests, × how many times): zwp_linux_dmabuf_v1 v4 ×28, wl_fixes v1 ×30, wl_compositor v6, wl_subcompositor v1, wl_output v4 ×2, zxdg_output_manager_v1 v3, wl_seat v7, wp_fractional_scale_manager_v1 v1, wp_viewporter v1, wl_shm v1, xdg_wm_base v6, xdg_activation_v1 v1, zwp_text_input_manager_v3 v1, zwp_relative_pointer_manager_v1 v1, zwp_pointer_constraints_v1 v1, wp_cursor_shape_manager_v1 v1, wp_presentation v2 ×24, wp_linux_drm_syncobj_manager_v1 v1 ×24, wp_color_manager_v1 v1 ×24, wp_commit_timing_manager_v1 v1 ×24, wp_fifo_manager_v1 v1 ×24, wl_seat v2, wl_data_device_manager v3, zwp_primary_selection_device_manager_v1 v1, wl_seat v10

Line timestamps are UTC time of day (HH:MM:SS.µs). The first line is at 2449.919834 s of the day; the smoke run's line (UTC) at 2450.202101: the first Wayland line is -282.3 ms before t=0. Run time = timestamp − 2450.202101.

Main surface wl_surface#29: 1888 commits (6 surfaces commit). Commit to the done of the frame callback requested with it: n 943, p50 15.7, p99 89.9, max 145.4 ms.

Restricted to frame callbacks done inside a JSON gap of 40 ms or more (to its end + 5 ms), 47 gaps: n 47, p50 56.2, p99 145.4, max 145.4 ms. done minus the latest listed probe completion (over_8ms) before it: n 47, p50 0.9, p5 0.0, p95 110.4 ms.

### strace

#### G-A

UNMAPs (decode-v0's AMDGPU_GEM_VA immediately followed by GEM_CLOSE): 22. Main-thread epoll_pwait of 40 ms or more: 57. UNMAP to the start of the next such wait: n 10, p50 137.3 ms, p5 16.1, p95 618.9; within 0–20 ms: 1 (10 %).

UNMAPs between the first and second decode-v0 SYNCOBJ wait of 10 ms or more after a GEM_CREATE: 2 of 22.

GEM_CREATE, GEM_VA and GEM_CLOSE per thread, counts in run seconds 0 to 20:

- decode-v0 789074 AMDGPU_GEM_CREATE: 153: 40 7 5 5 5 5 5 7 6 6 5 6 5 6 7 7 5 7 6 7 1
- decode-v0 789074 AMDGPU_GEM_VA: 298: 57 17 11 10 10 10 10 13 11 12 11 12 10 12 13 14 11 14 12 12 16
- decode-v0 789074 GEM_CLOSE: 147: 19 10 6 5 5 5 5 6 5 6 6 6 5 6 6 7 6 7 6 6 14
- doctor 789069 AMDGPU_GEM_CREATE: 218: 218 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789069 AMDGPU_GEM_VA: 430: 430 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789069 GEM_CLOSE: 209: 209 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789063 AMDGPU_GEM_CREATE: 195: 195 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789063 AMDGPU_GEM_VA: 728: 200 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 528
- spherecut 789063 GEM_CLOSE: 535: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 530
- spherecut:cs0 789075 AMDGPU_GEM_VA: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:cs0 789075 GEM_CLOSE: 5: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789077 AMDGPU_GEM_CREATE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789077 AMDGPU_GEM_VA: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789080 AMDGPU_GEM_CREATE: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789068 AMDGPU_GEM_CREATE: 322: 248 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789068 AMDGPU_GEM_VA: 323: 248 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1
- upload 789068 GEM_CLOSE: 1: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1

#### G-Ad

UNMAPs (decode-v0's AMDGPU_GEM_VA immediately followed by GEM_CLOSE): 24. Main-thread epoll_pwait of 40 ms or more: 0. UNMAP to the start of the next such wait: n 0, p50 – ms, p5 –, p95 –; within 0–20 ms: 0 (0 %).

UNMAPs between the first and second decode-v0 SYNCOBJ wait of 10 ms or more after a GEM_CREATE: 0 of 24.

GEM_CREATE, GEM_VA and GEM_CLOSE per thread, counts in run seconds 0 to 20:

- decode-v0 789265 AMDGPU_GEM_CREATE: 134: 34 8 4 4 4 4 4 7 7 6 8 5 4 4 4 6 7 4 5 4 1
- decode-v0 789265 AMDGPU_GEM_VA: 264: 50 13 12 9 9 8 7 13 15 10 18 10 8 8 7 13 14 8 10 7 15
- decode-v0 789265 GEM_CLOSE: 132: 17 6 8 5 5 4 3 6 8 4 10 5 4 4 3 7 7 4 5 3 14
- doctor 789260 AMDGPU_GEM_CREATE: 221: 221 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789260 AMDGPU_GEM_VA: 433: 433 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789260 GEM_CLOSE: 210: 210 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789254 AMDGPU_GEM_CREATE: 202: 198 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789254 AMDGPU_GEM_VA: 670: 203 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 463
- spherecut 789254 GEM_CLOSE: 470: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 465
- spherecut:cs0 789266 AMDGPU_GEM_VA: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:cs0 789266 GEM_CLOSE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789268 AMDGPU_GEM_CREATE: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789268 AMDGPU_GEM_VA: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789271 AMDGPU_GEM_CREATE: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789271 AMDGPU_GEM_VA: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789259 AMDGPU_GEM_CREATE: 315: 204 0 37 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789259 AMDGPU_GEM_VA: 380: 204 0 37 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 65
- upload 789259 GEM_CLOSE: 65: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 65

### Headless (H-*)

| arm | missed ticks | download p50 / p95 | drain p50 / p95 | shown per second, real time | shown_interval p50 | counts |
|---|---|---|---|---|---|---|
| H-off | 0 | 149.37/190.09 | – | 6.00 fps | 150.01 | yes |
| H-download | 0 | 66.05/67.18 | 91.72/153.25 | 5.00 fps | 183.33 | yes |
| H-picture | 0 | 65.53/66.85 | 141.82/214.45 | 4.00 fps | 216.66 | yes |

## 6. Verdict lines

| arm | kept | passes ≥ 95 ms | covered s | mid a second | presented (each) | p99 ms (each) | mean shown |
|---|---|---|---|---|---|---|---|
| A | 3 | 18 | 37.5 | 3.20 | 0.795, 0.783, 0.760 | 92.9, 100.7, 103.2 | 122.7 |
| A-end | 1 | 4 | 10.2 | 4.21 | 0.769 | 114.9 | 130.0 |
| Ad | 3 | 0 | 57.0 | 0.00 | 0.996, 0.997, 0.996 | 18.4, 18.0, 18.4 | 110.3 |
| Ap | 3 | 0 | 57.0 | 0.00 | 0.994, 0.993, 0.996 | 18.0, 18.2, 17.7 | 82.7 |
| B | 2 | 0 | 14.0 | 1.86 | 0.845, 0.819 | 64.2, 61.3 | 579.5 |
| Bp | 2 | 0 | 13.6 | 1.25 | 0.844, 0.826 | 62.7, 57.1 | 582.5 |
| P-A | 1 | 6 | 12.1 | 3.62 | 0.785 | 93.2 | 120.0 |
| P-Ad | 1 | 0 | 19.0 | 0.00 | 0.993 | 18.3 | 100.0 |

- **download mode passes:** Ad presented ≥ 0.95 in every kept run (0.996, 0.997, 0.996); Ad at most 2 passes ≥ 95 ms over ≥ 30 covered s (0 over 57.0 s); Ad mid under 0.5 a second (0.00); Ad p99 ≤ 25.0 ms in at least 2 runs (18.4, 18.0, 18.4); Ad mean shown ≥ 0.8 × A's (110.3 against 98.1); base-download's run 1 passed gui_decode_behind (failed [], inconclusive [])

- **picture mode fails:** Ap mean shown ≥ 0.8 × A's (82.7 against 98.1). Clauses met: Ap presented ≥ 0.95 in every kept run (0.994, 0.993, 0.996); Ap at most 2 passes ≥ 95 ms over ≥ 30 covered s (0 over 57.0 s); Ap mid under 0.5 a second (0.00); Ap p99 ≤ 25.0 ms in at least 2 runs (18.0, 18.2, 17.7); base-download's run 1 passed gui_decode_behind (failed [], inconclusive [])

- path A gaps: Bp mid 1.25 a second, B mid 1.86 (the line needs Bp under 1.0 and B 2.5 or more)

- **probe sees a gate:** P-A 44.53 latencies ≥ 30 ms a second; P-Ad p99 4.09 ms

- **Drift:** A-end 4 big over 10.2 s, mid 4.21 a second, presented 0.769, p99 114.9, shown 130; A-1..3 18 big over 37.5 s, mid 3.20, presented 0.795, 0.783, 0.760, p99 92.9, 100.7, 103.2, shown mean 123

Dropped: none


Note on the "picture mode" line: the brief's last clause names base-download's run 1. No official test ran with `picture`, so that clause was evaluated on base-download as the brief writes it.

Note on section 5's strace: "UNMAP" is decode-v0's `AMDGPU_GEM_VA` immediately followed on that thread by a `GEM_CLOSE`, among the traced calls (ioctl, epoll_pwait, munmap). strace slows every syscall, so G-A and G-Ad are not counted in section 6.
