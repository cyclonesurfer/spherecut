# odin: FFmpeg's Vulkan Video decode of clip 9, run after run

odin: RX 7600 (RADV, Mesa 26.2.5), system FFmpeg 8.1.3 (`/usr/bin/ffmpeg`, Fedora build). The clip is `samples/insta360-x5/VID_20261022_170245_00_009.insv`: two HEVC Main streams, 3840×3840, `yuvj420p` (pc, bt709), 30000/1001 fps, plus AAC. I compared the first 96 frames of each lens with `framemd5`, hash lines only. The Vulkan path outputs `nv12` (pc, bt709), so the software runs were converted to `nv12` (`-pix_fmt nv12`).

## The runs

| Runs | Command | Wall time each |
|---|---|---|
| `vk2-1..10` | both lenses at once on one Vulkan device (`-init_hw_device vulkan=vk -hwaccel vulkan -hwaccel_device vk`, `-map 0:v:0` and `-map 0:v:1`) | 3.44 to 3.48 s |
| `vk1-1..5` | lens 0 alone, same device options | 3.28 to 3.32 s |
| `sw-1..3` | software (`-hwaccel none`), both lenses, `-pix_fmt nv12` | 10.09 to 10.26 s |

Every command exited 0, with nothing on stderr.

## Result: every run is byte-identical

- **Lens 0 (`-a`):** all 10 `vk2` runs agree, all 5 `vk1` runs agree, and all 3 `sw` runs agree. `vk2`, `vk1` and `sw` also agree with each other: 0 of 96 frames differ.
- **Lens 1 (`-b`):** all 10 `vk2` runs agree, all 3 `sw` runs agree, and `vk2` matches `sw` (0 of 96 frames differ).
- **Sanity checks:**
  - Each lens has 96 distinct frame hashes.
  - The two lenses never share a hash.
  - pts runs 0 to 95 in timebase 1001/30000. That covers the frames `zero_copy_pair_matches_copy` named in PRs #29 and #30 (pts 3003 to 30030 in 1/30000, which is frames 3 to 30).

So FFmpeg's own Vulkan Video decode of both lenses at once is deterministic, and it's identical to software decode (after its `nv12` download) for these 96 frames. The run-to-run mismatch in `zero_copy_pair_matches_copy` doesn't come from FFmpeg's decoded bytes. Note that this only checks the downloaded `nv12` frames; it doesn't check Spherecut's path B, which uses the Vulkan images without downloading them.
