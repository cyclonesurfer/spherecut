# odin Tier 2 on proto/vaapi-drain with SPHERECUT_VAAPI_DRAIN=download (measure only)

## 1. Setup

- **Branch and code:** `proto/vaapi-drain` at **6675318**; the tree was clean. Every hw-test call ran with `SPHERECUT_VAAPI_DRAIN=download`, standing in for the coming default. env3.sh unset the stray variables, and env-before had only WAYLAND_DISPLAY and XDG_RUNTIME_DIR.
- **Other apps:** Firefox's browser was not running (only its `--dbus-service` helper, at 0 % CPU), and no audio stream was playing.
- **Session:** the idle inhibitor ("Spherecut stall Tier 2", sleep 14400) ran throughout. LockedHint was "no" before every step and at every 30 s check during the full run, so no lock occurred.
- **Machine:** in env.txt. It is the same block as last time: mutter/gnome-shell 50.5, Mesa 26.2.5 (freeworld VA driver), libva 2.23.0.
- **Runs:**
  - gdb-1..3 each exited 0 (16:57-17:07 PST).
  - The full Tier 2 exited 0 in 8471 s (17:07-19:28 PST).
  - Nothing was skipped beyond the one test that skips by itself, and nothing was dropped or rerun.

## 2. gui_decode_behind, three times (SPHERECUT_VAAPI_DRAIN=download)

| N | runs' failed | inconclusive | run 1 presented | run 1 UI p99 ms | run 1 shown / expected (ratio) | vaapi_drain | drain unavailable / errors / overflow | stages.drain n / p50 / p95 ms | stages.download p50 ms | run 0 presented / p99 | run 2 presented / p99 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 0: [], 1: [], 2: [] | [] | 0.995 | 18.3 | 108 / 600.4 (0.18) | download | 0 / 0 / 0 | 108 / 96.2 / 127.6 | 64.9 | 0.998 / 17.0 | 0.334 / 51.0 |
| 2 | 0: [], 1: [], 2: [] | [] | 0.994 | 18.5 | 107 / 600.0 (0.18) | download | 0 / 0 / 0 | 107 / 103.5 / 129.5 | 65.8 | 1.000 / 17.1 | 0.336 / 51.1 |
| 3 | 0: [], 1: [], 2: [] | [] | 0.995 | 18.1 | 102 / 600.4 (0.17) | download | 0 / 0 / 0 | 102 / 107.6 / 129.5 | 65.3 | 1.000 / 17.0 | 0.335 / 51.0 |

Run 1 passed its gates every time, presenting 0.994-0.995 (0.95 or more) with UI p99 18.1-18.5 ms. It stayed behind: shown 0.17-0.18 of expected, under the 0.85 the test allows. Runs 0 and 2 behaved as before, and run 2 failed as the control must.

Reference (hw-reports/odin-0b3ab9b4d0.md, no drain): gui_decode_behind **failed** on run 1, presenting 78 % with "UI p99 92.7 ms, over 1.5 × the 60 Hz period (25.0 ms)".

## 3. The whole Tier 2 against the reference

The reference is `hw-reports/odin-0b3ab9b4d0.md` on main (0b3ab9b, review-fix slice; no drain).

- **Reference:** Tier 2: 56 passed, 1 failed, 1 skipped (58 tests).
- **Today:** Tier 2: **59 passed, 0 failed, 1 skipped (60 tests)**; Tier 1 left out by `--no-tier1`.

### bench_playback_realtime_5k7_10bit, row "hevc10-5k7-30fps-noisy-20s, VA-API"

| | UI ticks missed | download p50 / p95 ms | drain p50 / p95 ms | shown per second, real time | shown_interval p50 ms |
|---|---|---|---|---|---|
| reference | 0 | 153.57 / 196.56 | – | 6.00 fps (4.00 to 11.00) | 166.67 |
| today | 0 | 64.83 / 67.07 | 103.89 / 127.68 | **4.00 fps (3.00 to 9.00)** | 200.00 |

Frames shown today are 4.00 / 6.00 = **0.67 of the reference's**, under the 0.8 the brief asks for. Both are medians of 20 one-second samples. For comparison, PR #37's H-download had 5.00 against H-off's 6.00.

### The other named rows

- **vaapi_render_matches_software:** pass, as in the reference.
- **zero_copy_matches_copy_path:** pass, as in the reference.
- **zero_copy_pair_matches_copy:** **pass**, as in the reference, which also passed. The RADV DCC fault does not show; M3.8 took the slot planes out of DCC, as PR #34 measured.
- **gui_smoke** (default Vulkan path, nothing drains): pass, 5 of 5, as in the reference. Each row is presented / UI p50, p99 / shown, dropped, decode-dropped:
  - noisy 5.7K 8-bit: 100 % / 16.7, 17.1 / 569, 0, 0 (reference identical);
  - noisy 5.7K 10-bit: 100 % / 16.7, 17.1 / 569, 0, 0 (reference p99 17.2);
  - seam 3840×1920: 100 % / 16.7, 17.2 / 269, 0, 0 (reference p99 17.0);
  - sync flash and beep: 100 % / 16.7, 17.1 / 275, 0, 0 (reference 274 shown);
  - X5 raw view: 100 % / 16.6, 17.1 / 1138, 0, 0 (reference 16.7, 17.1 / 1138).

### bench_seek_latency, VA-API rows (ms; reference → today)

| row | stat | reference | today | change |
|---|---|---|---|---|
| exact seek, Insta360 X5 1664x832 h264 at 29.97 fps (stream 0 of LRV_20261022_170245_01_009.lrv), VA-API | median | 17.49 | 17.69 | +1.1 % |
| exact seek, Insta360 X5 1664x832 h264 at 29.97 fps (stream 0 of LRV_20261022_170245_01_009.lrv), VA-API | p95 | 22.65 | 22.97 | +1.4 % |
| exact seek, Insta360 X5 3840x3840 hevc at 29.97 fps (stream 0 of VID_20261022_170245_00_009.insv), VA-API | median | 182.09 | 182.54 | +0.2 % |
| exact seek, Insta360 X5 3840x3840 hevc at 29.97 fps (stream 0 of VID_20261022_170245_00_009.insv), VA-API | p95 | 370.1 | 370.54 | +0.1 % |
| exact seek, noisy 5760x2880 HEVC 8-bit, VA-API | median | 131.97 | 133.79 | +1.4 % |
| exact seek, noisy 5760x2880 HEVC 8-bit, VA-API | p95 | 244.37 | 260.5 | +6.6 % |
| keyframe seek, Insta360 X5 1664x832 h264 at 29.97 fps (stream 0 of LRV_20261022_170245_01_009.lrv), VA-API | median | 4.35 | 4.43 | +1.8 % |
| keyframe seek, Insta360 X5 1664x832 h264 at 29.97 fps (stream 0 of LRV_20261022_170245_01_009.lrv), VA-API | p95 | 5.07 | 5.58 | +10.1 % |
| keyframe seek, Insta360 X5 3840x3840 hevc at 29.97 fps (stream 0 of VID_20261022_170245_00_009.insv), VA-API | median | 21.89 | 21.88 | -0.0 % |
| keyframe seek, Insta360 X5 3840x3840 hevc at 29.97 fps (stream 0 of VID_20261022_170245_00_009.insv), VA-API | p95 | 35.45 | 35.49 | +0.1 % |
| keyframe seek, noisy 5760x2880 HEVC 8-bit, VA-API | median | 49.86 | 55.41 | +11.1 % |
| keyframe seek, noisy 5760x2880 HEVC 8-bit, VA-API | p95 | 56.08 | 62.85 | +12.1 % |

The seek rows say only "VA-API"; none of them is a zero-copy row. The largest change is the keyframe seek on the noisy 5.7K 8-bit clip, at +11.1 % (median) and +12.1 % (p95). No row is more than 15 % slower.

### Every other test

Every other test has the same result as in the reference (pass), except:

- `tier2_gui::gui_decode_behind`: reference **FAIL**, today pass (section 2).
- `geometry::samples_calibrations_build_rigs`: absent from the reference, which predates it; today pass.
- `tier2_frame::samples_frame_clip8_renders`: absent from the reference, which predates it; today pass.
- `tier2_playback::bench_playback_stitched_samples`: skipped in both ("no stitched equirect among the samples … clip 12 is still to come").

### Verdict lines

- **gui_decode_behind passes 3/3**
- **headless VA-API row not within bounds:** frames shown 4.00 fps against the reference's 6.00, 0.67 of it, under 0.8. Its other numbers are inside the bounds: 0 UI ticks missed, download p50/p95 64.83/67.07 ms (reference 153.57/196.56), drain p50/p95 103.89/127.68 ms, shown_interval p50 200.00 ms (reference 166.67).
- **seek latency within +15 %**
- **no other change**: the two tests absent from the reference pass, and `gui_decode_behind` now passes.
