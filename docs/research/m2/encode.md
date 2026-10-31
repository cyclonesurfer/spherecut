# M2 export: encode research

Written 2026-10-31 for the M2 export work. Every claim below cites the source I opened. Anything marked
**unverified** is my inference or memory and has not been checked.

Trees I read (shallow, blobless clones, now deleted):
- FFmpeg tags `n6.1.1`, `n7.1`, `n8.1.3`, `n9.0.2` (github.com/FFmpeg/FFmpeg). Paths below are `file:line` at
  `n9.0.2` unless another tag is named.
- Mesa `mesa-26.2.5` (commit ddaf85e, "VERSION: bump for 26.2.5"). gitlab.freedesktop.org is blocked from here,
  so this came from the unofficial GitHub mirror github.com/chaotic-cx/mesa-mirror.
- Linux `v6.19`, `v7.0`, `v7.3` (github.com/torvalds/linux): `drivers/gpu/drm/amd/amdgpu/soc21.c`.
- RPM Fusion `ffmpeg` and `mesa-freeworld`, branch `f44` (github.com/rpmfusion/*). ffmpeg is at 154c0f3,
  "Update to 8.1.3". mesa-freeworld is at 5d1d761, which has already moved on to 26.2.6.
- google/spatial-media `master` (2308497, Jul 2026).
- Spherecut: `crates/media/src/{encode,inject,spherical,hw,hwprobe}.rs` and `ffi.rs`. `hwprobe.rs:184-305`
  already does a VA-API encode (frames context, get_buffer, transfer, encode). `ffi/vulkan.rs` already builds
  FFmpeg's Vulkan device from wgpu's VkDevice.

**Corrections (2026-10-31, from the M2 plan's review; m2-plan amendment 2).** The note below is as written,
apart from this list. Where they differ, this list wins.
- **Software encoders don't take NV12/P010** (finding 6). §1's recommendation of NV12/P010 holds for VA-API
  only. libx265 lists no NV12 or P010, libsvtav1 takes only `YUV420P` and `YUV420P10`, and libx264's 10-bit
  list is `YUV420P10, YUV422P10, YUV444P10, NV20` (`libx265.c:798-826`, `libsvtav1.c:698-700`,
  `libx264.c:1301-1306` at n6.1.1). The software path needs planar I420 and I010 (low bits).
- **The edit's duration is in the movie timescale** (finding 7). §6 says the 6.1 to 8.1 end is
  packet-granular and 9.0's end trim is exact. Neither is exact at FFmpeg's default `movie_timescale` of 1000
  (`MOV_TIMESCALE`), where one tick is 48 samples at 48 kHz: `elst` durations are rescaled with
  `AV_ROUND_UP` (`movenc.c:3540-3542` at n6.1.1). Muxed with the box's 6.1.1 (48,485 AAC samples), the
  default gives mvhd 1000 and `elst [(1010, 1024)]`, 48,480 samples, 5 short; `-movie_timescale 48000` gives
  `[(48485, 1024)]`, exact (`scripts/elst.py`).
- **Video gets an edit list too** (finding 8a). `use_editlist` resolves to 1 for every track, so a video
  track has one: `[(1000, 1024)]` with libx264's B-frames, `[(1000, 0)]` with `-bf 0` or
  `+negative_cts_offsets` (`movenc.c:3796-3798`, `:7029-7030` at n6.1.1; `scripts/elst.py`). A YouTube export
  that wants no video edit must remove it itself.
- **faststart can't run on our writer** (finding 8b). Its second pass reopens `s->url` for reading
  (`mux_utils.c:79-81` at n6.1.1), and `Mp4Writer` muxes into an AVIO stream with no URL. The plan moves moov
  to the front in its own pass.
- **§1's av1_vaapi default-QP note applies to CQP only** (nit 49); under VBR the bitrate rules.
- **The Opus caveat in §6 stands**: whether the edit's `media_time` includes PreSkip is still unverified,
  and M2 tests Opus copy or greys it out.

---

## Short answers

1. **VA-API encode setup (same in 6.1 to 9.0).** Create a VAAPI device. Make a frames context with
   `format=AV_PIX_FMT_VAAPI`, `sw_format=NV12|P010` and the size. Give the encoder `pix_fmt=VAAPI` and
   `hw_frames_ctx=<ref>`. `hw_device_ctx` alone is refused. For each frame call `av_hwframe_get_buffer` and
   then `av_hwframe_transfer_data` from an NV12/P010 system frame. The sw_format's bit depth selects the
   profile.
   **Zero copy:** a DRM PRIME frame can be mapped into a VAAPI frame with `av_hwframe_map` (the VAAPI frames
   context's `map_to` from DRM_PRIME). The frame must be one dmabuf object in NV12 (or R8+GR88) or P010
   (R16+RG1616) layers. The source frame needs no frames context. radeonsi then refuses DCC-compressed
   surfaces, and we must fence the Vulkan writes ourselves. The code path exists and is the same in
   6.1 and 9.0. **Untested on RADV+radeonsi.**
   **Renames:** the user-facing option names did not change. The 7.1 `hw_base_encode` refactor moved
   `idr_interval`, `b_depth` and `async_depth` without renaming them. `blbrc` is new in 7.x. 8.0 removed
   `FF_PROFILE_*`, `AVCodecContext.ticks_per_frame` and `AVFrame.key_frame`. 9.0 removed `AVCodec.pix_fmts`;
   use `avcodec_get_supported_config` (7.1+).
2. **radeonsi VA-API encode on VCN 4 (Navi 33 and Strix Halo alike).**
   - H.264: CBP, Main and High, **8-bit only**, max **4096×4096** (the kernel's limit, which replaces Mesa's
     4096×2304), B-frames yes.
   - HEVC: Main and **Main10**, max **8192×4352**, **no B-frames**.
   - AV1: Main, 8-bit and **10-bit**, max **8192×4352**, **no B-frames on VCN 4** (VCN 5 has them).
   - Rate control: **CQP, CBR, VBR**, plus **QVBR** if the VCN 4 encoder firmware's minor version is 15 or
     higher. **No ICQ, no AVBR.**
   - Speed/quality: `-compression_level` (h264_vaapi also has `quality`). Mesa reads it as a bit field:
     preset speed/balanced/quality/high-quality, pre-encode, and VBAQ (adaptive quantisation). The range is 0–32.
   - Consequence: **5760×2880 and 7680×3840 hardware encode is HEVC or AV1 only.**
3. **Vulkan Video encode.**
   - FFmpeg: `h264_vulkan` and `hevc_vulkan` arrived in **7.1**, `av1_vulkan` in **8.0**. There are none in
     6.1.
   - RADV 26.2.5: **on by default** where the VCN encoder firmware can write memory (VCN 4 needs firmware
     minor version 22 or higher; VCN 5 always can). Otherwise it needs `RADV_EXPERIMENTAL=video_encode`
     (`RADV_PERFTEST=video_encode` is a deprecated alias).
   - Which codecs are built is a Mesa build option. The default, `all_free`, builds AV1 encode only.
     RPM Fusion's freeworld builds H.264, HEVC and AV1. So odin (vulkan-freeworld) can have all three and
     huginn (Fedora's RADV) AV1 only. **Huginn's Fedora build flags are unverified.**
4. **Software fallback.**
   - Options: libx264 `preset`/`tune`/`crf`/`x264-params`; libx265 `preset`/`tune`/`crf`/`x265-params`;
     libsvtav1 `preset` (int)/`crf` 0–63/`svtav1-params`. libsvtav1's `la_depth`, `tile_*`, `sc_detection` and
     `tier` options exist in 6.1 but are gone by 7.1.
   - RPM Fusion's 8.1.3 for F44 x86_64 builds with `--enable-libx264 --enable-libx265 --enable-libsvtav1`.
     It also enables Vulkan and libfdk-aac.
   - Fedora 44's `libavcodec-free` 8.0.1 depends on svt-av1-libs, libaom, rav1e and noopenh264, but **not**
     on x264 or x265.
   - Speeds on 16 Zen 5 cores (Bosphorus 4K, raw input): SVT-AV1 preset 8 runs at **126 fps** (9950X,
     SVT-AV1 2.3); x265 3.6 at **37.6 fps** (9950X, preset not stated). x265 4.1's best results are about
     **45–47 fps** across all CPUs.
5. **Levels.**
   - H.264: 3840×2160@30 needs **5.1**; 5760×2880@30 and 7680×3840@30 need **6.0**. H.264's largest frame is
     139,264 MBs, about 35.6 Mpx (level 6.x), so 7680×3840 is legal H.264. AMD's hardware just can't encode it.
   - HEVC: 4K30 needs **5.0**; 5760×2880 and 7680×3840 need **6.0**.
   - AV1: 4K30 needs **5.0**; 5760×2880 and 7680×3840 need **6.0**. 7680×3840 fits 5.x's 8192×4352 width
     and height limits but exceeds its 8.9 Mpx picture-size limit.
   - YouTube: MP4 with fast start, H.264 High, 35–45 Mb/s for 4K30 and 80–160 Mb/s for 8K30 (SDR), AAC-LC or
     Opus at 48 kHz.
   - YouTube says **"No Edit Lists"**.
   - **I found no official maximum 360° resolution.**
   - YouTube links the spatial-media injector. Its CLI and GUI write **V1 XMP by default** and V2 only with
     `--v2`. Spatial audio is ambiX (ACN/SN3D) W,Y,Z,X in 4 channels at 48 kHz: AAC needs at least 256 kb/s;
     Opus needs channel mapping family 2 and at least 512 kb/s.
6. **Sample-exact audio passthrough.**
   - Stamp the copied packets with the output timeline. Packets before the cut get negative pts in
     1/sample_rate. FFmpeg's mov muxer (`use_editlist` default auto→1 when not fragmented) then writes `elst`
     with `media_time = −first_dts`, which trims the start exactly in 6.1–9.0.
   - Start one AAC packet before the cut (roll −1) and 80 ms before for Opus. The muxer writes those `roll`
     groups itself.
   - **Exact end trims through the muxer exist only in 9.0.** It honours `AV_PKT_DATA_SKIP_SAMPLES` end
     padding and `AV_PKT_FLAG_DISCARD`. In 6.1–8.1 the edit ends on a packet boundary, unless we patch
     `elst` ourselves, as `inject.rs` could.
   - PCM can be cut exactly with no edit list at all (`ipcm`/`fpcm` in MP4 since 6.1).
   - Re-encoding: the native `aac` encoder ranks below libfdk_aac. For 4-channel ambiX label the audio
     `4.0` or `quad`/`2.2`, since an ambisonic layout is refused. Turn off `aac_is` and `aac_pns`. Use
     256 kb/s or more.

---

## 1. VA-API encode through libavcodec

**Setup.** Spherecut's `hwprobe::encode_one` already does exactly this.
- Device: `av_hwdevice_ctx_create(VAAPI, "/dev/dri/renderD12x")`. `ffi.rs:738` already does this.
- Frames context: `av_hwframe_ctx_alloc(dev)`, then set `format=AV_PIX_FMT_VAAPI`, `sw_format=NV12` or `P010`,
  and `width`/`height`, then `av_hwframe_ctx_init`.
  - `initial_pool_size=0` gives a dynamic pool (`libavutil/hwcontext_vaapi.c:541-567`, same at 6.1.1:508-534).
    A fixed pool must cover `async_depth` (default 2), the B-frame lookahead, and the frames we keep in flight.
- Encoder: `pix_fmt=AV_PIX_FMT_VAAPI` and `hw_frames_ctx=av_buffer_ref(frames)` before opening.
  - Encoders accept only `HW_CONFIG_ENCODER_FRAMES(VAAPI, VAAPI)` (`vaapi_encode.c:36-39`; 6.1.1:34-37).
  - Without frames, init fails with "A hardware frames reference is required to associate the encoding
    device" (`hw_base_encode.c:789-793`; 6.1.1 `vaapi_encode.c:2776`). The device is taken from
    `frames->device_ref`, never from `hw_device_ctx`.
  - Set `AV_CODEC_FLAG_GLOBAL_HEADER` for MP4, as `Mp4Writer` already does.
- Profile: chosen from the sw_format's depth and chroma (`vaapi_encode.c:935-975`), so P010 selects HEVC Main10
  or AV1 10-bit.
  - FFmpeg lists `H264High10` when built against VA-API 1.18 or newer (`vaapi_encode_h264.c:948-957`), but
    Mesa refuses it (§2).
- Per frame: `av_hwframe_get_buffer(frames, surf, 0)`, then `av_hwframe_transfer_data(surf, sw, 0)`, copy
  `pts`, and `avcodec_send_frame`. The encoder reads the surface id from `frame->data[3]`
  (`vaapi_encode.c:847`) and keeps a reference until the packet is out.
- Defaults: `bf=2`, `g=120`, `b=0` (`vaapi_encode_h264.c:1146-1158`, `_h265.c:1177-1189`, `_av1.c:1024-1032`).
  - With no bitrate the encoder uses CQP. The default QP is 20 for h264 and 25 for hevc
    (`_h264.c:967`, `_h265.c:1036`).
  - av1 also defaults to 25, but on a 0–255 q_idx scale (`_av1.c:851`, `:140`). That is very high quality,
    so set it explicitly.

**Options (same names 6.1 to 9.0).**
- Common: `low_power`, `max_frame_size`, `idr_interval`, `b_depth`, `async_depth`, `rc_mode`
  (auto/CQP/CBR/VBR/ICQ/QVBR/AVBR).
  - At 6.1 these were all in `VAAPI_ENCODE_COMMON_OPTIONS` (`vaapi_encode.h:498-537` at n6.1.1).
  - From 7.1, `idr_interval`, `b_depth` and `async_depth` live in `HW_BASE_ENCODE_COMMON_OPTIONS`
    (`hw_base_encode.h:243-255`), with the rest in `vaapi_encode.h:354-384`.
  - `blbrc` (block-level rate control) is in 7.1 but not 6.1. The 9.0 `async_depth` help text no longer
    mentions vaSyncBuffer.
- h264_vaapi: `qp quality coder aud sei profile level`.
- hevc_vaapi: `qp aud profile tier level sei hdr tiles`.
- av1_vaapi: `profile tier level tiles tile_groups`.
- Speed/quality is `AVCodecContext.compression_level`, sent to the driver only if ≥0 (`vaapi_encode.c:2218`,
  `:1975-2020`). h264's `quality` option just sets it (`_h264.c:1004`).
- Rate-control choice in `auto` (`vaapi_encode.c:1288-1350`):
  - an explicit `rc_mode` must be supported by the driver;
  - QSCALE or an explicit `qp` gives CQP;
  - bitrate + global_quality gives QVBR;
  - global_quality alone tries ICQ, then CQP;
  - bitrate == maxrate gives CBR;
  - bitrate alone tries AVBR, then VBR, then CBR;
  - nothing set tries ICQ, then CQP.

**API changes that touch an encoder wrapper**, checked by grepping the headers at each tag:
- `FF_PROFILE_*`, `AVCodecContext.ticks_per_frame` and `AVFrame.key_frame` are present in 6.1 and 7.1 and gone
  in 8.1.3. `AV_PROFILE_*` and `AV_FRAME_FLAG_KEY` exist in all four versions.
- `AVCodec.pix_fmts` is gone in 9.0.2. `avcodec_get_supported_config` exists from 7.1, so 6.1 needs a cfg.
- `av_hwframe_map` and `AV_HWFRAME_MAP_*` are unchanged in all four.

**DRM PRIME to VAAPI without a copy.**
- The call: `av_hwframe_map(dst, src, flags)` with `dst->format=AV_PIX_FMT_VAAPI` and
  `dst->hw_frames_ctx=<the encoder's VAAPI frames>`. `src->format` is `AV_PIX_FMT_DRM_PRIME`, and
  `src->data[0]` holds an `AVDRMFrameDescriptor` that `src->buf[0]` owns.
- `av_hwframe_map` tries the source frames context's `map_from` only if the source has one, then the
  destination's `map_to` (`hwcontext.c:793-860`). VAAPI's `map_to` handles DRM_PRIME (`hwcontext_vaapi.c:1625-1635`).
- `vaapi_map_from_drm` (`hwcontext_vaapi.c:1109-1350`; 6.1.1:1069) reads its size and its PRIME_2 cache from
  the *destination* frames context. So `src` needs no `hw_frames_ctx`; this is my reading of the code
  (**not run**).
- What it requires and does:
  - `nb_objects == 1`, otherwise "VAAPI can only map frames made from a single DRM object" (`:1148-1152`).
    A disjoint Vulkan image fails.
  - Layers must match `vaapi_drm_format_map` (`:1060-1090`): NV12 as one `NV12` layer or as `R8`+`GR88`/`RG88`;
    P010 as `R16`+`RG1616`; ARGB/XRGB/ABGR/XBGR 8888 are accepted too.
  - It imports with `VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2` (with the modifier) when the modifier is valid,
    else it falls back to the old PRIME import.
  - It calls `vaCreateSurfaces` per map and destroys that surface on unmap. The same code is in 6.1.1.
- Driver side: VCN reads the input in its own swizzle mode but refuses DCC.
  - "DCC surfaces not supported." (`radeonsi/mm/radeon_vcn_enc_4_0.c:402`). EFC, the encoder's colour
    conversion, also bails on DCC (`radeon_vcn_enc.c:1927`).
  - So export with a non-DCC modifier; LINEAR is safest. **Which GFX11 tiled modifiers work is unverified.**
- Sync is ours to do. FFmpeg's own Vulkan→DRM export either exports a sync_file from the semaphores and
  imports it into the dmabuf (`DMA_BUF_IOCTL_IMPORT_SYNC_FILE`) or CPU-waits on them
  (`hwcontext_vulkan.c:4226-4290`).
  - FFmpeg can also map its own Vulkan frames straight to VAAPI (`vulkan_map_from` → `vulkan_map_to_vaapi`,
    `:4391-4415`, present in 6.1.1 too). That needs DRM-modifier tiling (`FF_VK_EXT_DRM_MODIFIER_FLAGS`).
- Is it worth it? Readback alone is cheap. NV12 is 12.4 MB a frame at 4K (373 MB/s at 30 fps) and 44 MB a
  frame at 7680×3840 (1.33 GB/s). That is small next to PCIe 4.0 x8 or Strix Halo's unified memory.
  - Recommendation: convert to NV12/P010 in a wgpu compute pass, read back, upload, and leave dmabuf mapping
    for later. Rendering straight into FFmpeg Vulkan frames for Vulkan encode (§3) is the other zero-copy route.

## 2. Mesa 26.2.5 radeonsi VA-API encode on VCN 4

Where the capabilities come from:
- `src/amd/common/ac_video.c:390-523` (`vcn_enc_caps`) sets them per codec.
- `ac_video.c:664-702` then **overwrites max width and height with the kernel's**
  `AMDGPU_INFO_VIDEO_CAPS_ENCODE` values.
- `si_mm_screen.c:102-290` exposes them to the VA frontend.
- Kernel `soc21.c:52-56` (identical at v6.19, v7.0 and v7.3) gives every VCN 4.0.x (4.0.0/2/4/5 at
  `:177-206`, 4.0.6 at `:208-212`) AVC 4096×4096, HEVC 8192×4352 and AV1 8192×4352.

| | H.264 | HEVC | AV1 |
|---|---|---|---|
| Profiles (si_mm_screen.c:109-123) | CBP, Main, High; **no High10** | Main, Main10 (VCN≥2), MainStill | Main, 8 and 10-bit (`formats.p010=1`) |
| Max size (kernel) | **4096×4096** | **8192×4352** | **8192×4352** |
| Min size / alignment | 128×128 / 16×16 | 130×128 / 64×16 | 128×128 / 64×16 (VCN5: 8×2) |
| Refs (L0/L1) → B-frames | 2/1 → **yes** (VCN≥3) | 1/0 → **no** | 1/0 → **no on VCN4**, yes on VCN5 |
| Max level (Vulkan caps) | 5.2 | 6.1 | 6.1 |
| QP range | 1–51 (VCN4) | 0–51 | 1–255; **8–255 on VCN 4.0.2/4.0.5/4.0.6** (`:518`) |

Our sizes fit HEVC and AV1. Widths 3840, 5760 and 7680 are all multiples of 64, and heights 2160, 2880 and
3840 are multiples of 16.

**Rate control.**
- `VAConfigAttribRateControl` is always `VA_RC_CQP|CBR|VBR`. `QVBR` is added when the caps allow it
  (`frontends/va/config.c:272-283`).
- QVBR needs VCN 4 encoder firmware minor version 15 or higher (VCN 5: 3 or higher; `ac_video.c:26-37`).
- ICQ and AVBR are never reported, so FFmpeg's `auto` lands on CQP (no bitrate) or VBR (bitrate set).

**Quality level.**
- Mesa reports `VAConfigAttribEncQualityRange` = 32 (`si_mm_screen.c:139`).
- It reads the value as bits (`va_private.h:445-456`, `va/encode.c:13-55`):
  - bit 0: valid;
  - bits 1–2: preset (0 speed, 1 balanced, 2 quality, 3 high quality; high quality is VCN 4+ only,
    `radeon_vcn_enc.c:20-49`);
  - bit 3: 4× pre-encode;
  - bit 4: VBAQ.
- 0 means speed with no extras. 1 means balanced + pre-encode + VBAQ. Two examples: 7 is high quality alone,
  31 is high quality + pre-encode + VBAQ.
- The HEVC speed preset is bumped to balanced on VCN 2–4 while SAO is on. QVBR forces pre-encode.

**Other notes.**
- RGB input can be converted to NV12/P010 in the encoder (EFC, VCN ≥2). It is reached only through VA's video
  processing pipeline (libavfilter `scale_vaapi`), which we don't link (`radeon_vcn_enc.c:1894-1952`).
- Navi 33 and Strix Halo are both VCN 4.0.x and get identical tables apart from AV1's min QP. I could not
  check which 4.0.x Strix Halo reports, or the firmware minor versions that gate QVBR and RADV encode.
  `AMD_DEBUG=info` prints `vcn_enc_major/minor_version` (`ac_gpu_info.c:2038-2040`), so doctor can check.
- **No credible VCN 4 throughput figure for 4K or 8K found; measure it.**

## 3. Vulkan Video encode

**FFmpeg.**
- `Changelog` (n9.0.2): 7.1 added "Vulkan H.264 encoder" and "Vulkan H.265 encoder" (`:289-290`); 8.0 added
  "AV1 Vulkan encoder" (`:262`). The `vulkan_encode*.c` files are absent at n6.1.1, h264/h265 appear at n7.1,
  and av1 at n8.1.3.
- 9.0 also added a `v360_vulkan` filter (`:184`). It isn't needed here, but it is relevant to 360 work.
- Requirements:
  - only `HW_CONFIG_ENCODER_FRAMES(VULKAN, VULKAN)` and a `hw_frames_ctx` (`vulkan_encode.c:27`, `:769-773`);
  - `avctx->sw_pix_fmt`;
  - the device extensions VK_KHR_video_encode_queue, VK_KHR_video_maintenance1 and the codec's (`:792-800`).
- Options (`vulkan_encode.h`): `qp`, `quality`, `rc_mode` (auto/driver/cqp/cbr/vbr), `tune`
  (default/hq/ll/ull/lossless), `usage`, `content`, plus the common `idr_interval`/`b_depth`/`async_depth`.
- Fit with our code: `ffi/vulkan.rs` already hands wgpu's VkDevice to FFmpeg. To encode on that device, wgpu's
  device would need the encode queue family and those extensions, and frames would be rendered into FFmpeg
  Vulkan frames. That would be zero copy on 7.1+ only. **Unverified that wgpu-hal device creation can add
  them.**

**RADV in Mesa 26.2.5.**
- `radv_probe_video_encode` (`radv_video_enc.c:68-84`) enables encode when the VCN_ENC queue's
  `write_memory != NONE`, which RADV needs for SetEvent and conformance (CTS). Otherwise it needs
  `RADV_EXPERIMENTAL=video_encode`.
- `write_memory_supported` (`ac_video.c:104-121`): VCN 5 always; VCN 4 when the encoder firmware minor
  version is 22 or higher.
- `docs/envvars.rst:1628-1653`: `RADV_PERFTEST=video_encode` is deprecated in favour of `RADV_EXPERIMENTAL`.
- Codec extensions also need build-time `VIDEO_CODEC_*ENC` (`radv_physical_device.c:827-836`).
  - Mesa's `video-codecs` default is `all_free`, the non-patent-encumbered codecs (`meson.options:725-737`).
  - RPM Fusion's `mesa-freeworld.spec:352` builds `h264enc,h265enc,av1enc` (among others) and ships
    `mesa-vulkan-drivers-freeworld`.
  - So odin can do RADV H.264/HEVC/AV1 encode, and huginn on Fedora's RADV only AV1 (if Fedora uses the
    default; src.fedoraproject.org is blocked, **unverified**).
- RADV also exposes `VK_VALVE_video_encode_rgb_conversion` on EFC hardware (`radv_physical_device.c:991-992`).
  FFmpeg 9.0.2's `vulkan_encode.c` doesn't reference it.

## 4. Software fallbacks

**Option names.** Grepped from each encoder's AVOption table at all four tags.
- libx264, identical 6.1→9.0: `preset tune profile level crf crf_max qp aq-mode aq-strength psy psy-rd
  rc-lookahead weightb weightp b-pyramid mbtree … x264opts x264-params`.
- libx265: `crf` (float), `qp`, `forced-idr`, `preset`, `tune`, `profile`, `udu_sei`, `a53cc`, `x265-params`.
  9.0 adds `x265-stats` and `dolbyvision`.
- libsvtav1 at 9.0, 8.1 and 7.1: `preset` (int, −2..MAX, default −2 = library default), `crf` 0–63, `qp` 0–63,
  `svtav1-params`, `dolbyvision`. Defaults are `g=-1` and `+cgop`.
  - 6.1.1 also has `hielevel la_depth tier sc_detection tile_columns tile_rows`. They were removed by 7.1,
    so use `svtav1-params`.
- configure minimums (`configure`): x264 `X264_BUILD >= 155` (7.1+; 122 at 6.1), x265 `X265_BUILD >= 89`,
  SvtAv1Enc ≥ 0.9.0.
- The cloud box has FFmpeg 6.1.1 with x264 0.164, x265 3.5 and SVT-AV1 1.7.0 (`pkg-config`).

**What's linked.**
- RPM Fusion `ffmpeg.spec` (f44, 8.1.3-1):
  - `%ifnarch i686` sets `_with_svtav1` (and rav1e, vvenc, placebo); x86_64 adds `_with_vpl`.
  - configure has `--enable-libx264 --enable-libx265 --enable-libsvtav1 --enable-vulkan --enable-libshaderc
    --enable-libfdk-aac --enable-libdrm --enable-gpl`.
  - `BuildRequires: AMF-devel` (`:119`; changelog "Enable AMF support", 2022), so FFmpeg's configure
    auto-detects `h264_amf`/`hevc_amf`/`av1_amf`. Those need AMD's proprietary AMF runtime, not Mesa
    (**unverified on Fedora 44**).
  - VA-API comes from configure auto-detection: the spec only adds `BuildRequires: libva-devel` (`:178`).
- Fedora 44 `libavcodec-free` 8.0.1-6.fc44 (packages.fedoraproject.org):
  - its dependencies include `svt-av1-libs`, `libaom`, `rav1e-libs`, `noopenh264`, `libva`, `libvpl`,
    `libshaderc` and `fdk-aac-free`;
  - **no x264 or x265**.
  - Note that Fedora 44 ships ffmpeg-free 8.0.1, not 8.1.

**Speeds.** These are encoder-only runs on a raw `.y4m` Bosphorus 4K (3840×2160).
- SVT-AV1 2.3, preset 8: **126.4 fps** on a Ryzen 9 9950X (openbenchmarking 2501054-NE-9950XSVTA47).
- SVT-AV1 4.0, preset 8: **152.6 fps** on a 9950X3D2 (2605017-NE-9950X3D2C40).
- x265 3.6: **37.6 fps** on a 9950X (2408222-NE-M1446523427; the page doesn't give the preset).
- x265 4.1: the pts/x265 test page shows the top results at about 45–47 fps across 130 runs, not tied to CPUs.
- Phoronix's Ryzen AI Max+ 395 review says the 395's video-encode results against the 9950X were "mixed"
  (charts only, no numbers in the text).
- Rough expectation: 4K30 runs at or above real time with SVT-AV1 preset 8 or higher and with x265 `fast` or
  faster. 7680×3840 is about 3.6× the pixels, so x265 would not keep up. **No x264 4K figure found.**

## 5. Levels and YouTube

The level tables are from FFmpeg: H.264 `libavcodec/h264_levels.c:24-49`, HEVC `h265_profile_level.c:22-40`,
AV1 `av1_levels.c:26-45`. The checks follow `ff_h264_guess_level` (MaxFS, MaxMBPS, and width²/height² ≤ 8·MaxFS).

| @30 fps | H.264 (MBs; MB/s) | HEVC (luma px; px/s) | AV1 |
|---|---|---|---|
| 3840×2160 | **5.1** (32,400 ≤ 36,864; 972,000 ≤ 983,040) | **5.0** (8.29M ≤ 8.91M; 249M ≤ 267M) | **5.0** |
| 5760×2880 | **6.0** (64,800 > 36,864 rules out 5.2) | **6.0** (16.6M > 8.91M) | **6.0** (16.6M > 8.91M) |
| 7680×3840 | **6.0** (115,200 ≤ 139,264; 3.46M ≤ 4.18M) | **6.0** (29.5M ≤ 35.65M; 885M ≤ 1.07G) | **6.0** |

- H.264's ceiling is level 6.x MaxFS = 139,264 MBs (35.65 Mpx; 8192×4320 fits). So 7680×3840 is legal H.264.
  - libx264 can make it. VCN can't (4096 wide).
  - Many hardware decoders stop at level 5.2 / 4096 wide (**unverified per device**), so H.264 is a poor
    choice for 360 above 4K.
- AV1 5.x allows 8192×4352 dimensions but only 8,912,896 px, so 7680×3840 is 6.0.

**YouTube.**
- support.google.com/youtube/answer/1722171:
  - Container: MP4, "No Edit Lists (or the video might not get processed correctly)", moov at the front
    (fast start).
  - Audio: AAC-LC, Opus or Eclipsa Audio at 48 kHz (384 kb/s stereo).
  - Video: H.264 High, 2 consecutive B-frames, closed GOP of half the frame rate, CABAC, 4:2:0.
  - SDR bitrates: 2160p30 35–45 Mb/s, 8K30 80–160 Mb/s. HDR: 44–56 and 100–200 Mb/s.
  - The page doesn't mention HEVC or AV1 at all.
- answer/6178631 ("Upload 180- or 360-degree videos"): no resolution limits. It links answer/1722171 and the
  injector release spatial-media `v2.0`, and says 360° playback "may take up to an hour".
  - **No official maximum 360 resolution found.** answer/6396222 (live) only says 2160p or 1440p is preferred.
- answer/6395969 (spatial audio):
  - ambiX ACN/SN3D, W,Y,Z,X as 4 channels (or 6 with head-locked L,R) at 48 kHz;
  - AAC in MP4/MOV at 256 kb/s or more;
  - Opus in MP4 with "Channel mapping family: 2" at 512 kb/s or more (768 with head-locked);
  - PCM in MOV;
  - one audio track.
- spatial-media `spatialmedia/__main__.py`: V2 boxes only with `--v2`, otherwise V1 XML. The GUI
  (`gui.py:148`) writes V1. `metadata_utils.inject_mpeg4` writes V1 `uuid`, V2 `st3d`/`sv3d` and `SA3D`.
  - **Whether YouTube accepts V2-only files is unverified.** The cheap safe choice is to write V1 `uuid` and
    V2 together for YouTube exports; `spherical.rs` already parses V1.
- Edit lists against YouTube's "No Edit Lists": an FFmpeg-encoded AAC track carries an `elst` for its
  1024-sample priming anyway. **Whether YouTube really rejects that is unverified.** A "YouTube" preset could
  set `use_editlist=0`; the mov muxer then shifts timestamps to zero (`movenc.c:8222-8224`).

## 6. Audio passthrough with exact trims into MP4

How FFmpeg's mov muxer writes the edit:
- Option `use_editlist`: −1 means auto (`movenc.c:122`; 6.1.1:108), resolved to 1 unless fragmented without
  `delay_moov` (`:8203-8214`; 6.1.1:7029).
- `mov_write_edts_tag` (`:4133-4228`; 6.1.1:3537):
  - for a track whose first dts is negative, it writes one entry with `media_time = -start_dts`
    (`start_ct = -FFMIN(start_dts, 0)`, `:4205`; 6.1.1:3609) and the pts duration;
  - a positive start adds an empty edit first.
- The muxer never reads `codecpar->initial_padding` for AAC. The edit comes only from timestamps.
  - FFmpeg's encoders emit their priming as negative pts: `aac` sets `initial_padding=1024` (`aacenc.c:1165`),
    and the first packet comes out at pts −1024.
  - That is why today's `Mp4Writer.edit_start` (negative video pts) works.
- `roll` sample groups: `mov_preroll_write_stbl_atoms` writes AAC with roll distance −1 and Opus with an 80 ms
  roll (`:3305-3436`, also at 6.1.1:2719).

Recipe for copying packets with a cut at sample S of the source (in samples, time base 1/sample_rate):
- **AAC:**
  - Start at the packet *before* the one containing S. AAC needs one packet of pre-roll (the overlapping MDCT
    window).
  - Give each copied packet `pts = src_pts − S`, so the first one or two are negative. The muxer then writes
    `media_time = S − pts_first`, which is exact.
  - Source pts are already on the source's post-edit timeline: FFmpeg's demuxer turns the source's own audio
    `elst` into `skip_samples` (`mov.c:4549-4553`), and at 9.0 into `codecpar->initial_padding` (`:4669`).
    So S counts presented samples.
- **The end.**
  - 6.1–8.1.3: the `elst` duration runs to the end of the last packet's pts (`calc_samples_pts_duration`),
    so the end is packet-granular, up to 21.3 ms at 48 kHz.
  - **9.0 only:** `AV_PKT_DATA_SKIP_SAMPLES` with end padding (bytes 4..7) shortens the edit (`:7313-7321`).
    Packets flagged `AV_PKT_FLAG_DISCARD` are stored but kept out of the edit's end (`elst_end_pts`,
    `:7343-7344`). Neither exists at n8.1.3, by grep.
  - Portable option: after muxing, rewrite `elst.segment_duration` with `inject.rs`. It is a same-size field
    patch.
  - On read, FFmpeg's demuxer keeps the packet that crosses the edit end whole (`mov.c:4622-4625`), so our
    own player must apply the end itself.
- **Opus:**
  - Same timestamps, with about 80 ms of pre-roll (4 packets of 20 ms) before S.
  - `dOps.PreSkip` is copied from extradata (`movenc.c:882-895`).
  - **Unverified:** whether the edit's `media_time` should include PreSkip or sit on top of it, and how
    players combine the two.
  - FFmpeg's libopus wrapper doesn't validate mapping family 2 (ambisonics); it warns "Unknown channel mapping
    family" and passes the value through to libopus (`libopusenc.c:222-262`).
- **PCM:**
  - Each sample is independent: split the first and last packets at S and E and copy the rest. No edit list
    is needed.
  - FFmpeg's mp4 muxer writes ISO `ipcm`/`fpcm` from 6.1 (`movenc.c:7868-7878` at 6.1.1).
  - YouTube asks for PCM in MOV, and player support for `ipcm` in MP4 is uneven (**unverified per player**).

Re-encoding instead, with the native `aac` encoder:
- Quality: FFmpeg's wiki (fftrac-bg.ffmpeg.org/wiki/Encode/AAC) ranks it "aac_at ≥ libfdk_aac > Native FFmpeg
  AAC encoder (aac)". It calls VBR `-q:a` "experimental". Its rule of thumb is about 64 kb/s per channel.
  So 192–256 kb/s is comfortable for stereo; **I found no listening-test numbers**.
  - libfdk_aac is in RPM Fusion's build, and Fedora's ffmpeg-free depends on `fdk-aac-free`.
- Defaults: `bit_rate=0` gives 128k per channel pair and 69k per single channel (`aacenc.c:1199-1204`).
  The default coder is `twoloop`.
- Options in 9.0: `aac_coder aac_ms aac_is aac_pns aac_tns aac_pce`. `aac_ltp` and `aac_pred` exist in 6.1
  and 7.1 but are gone in 8.1.3.
- 4-channel ambiX:
  - Built-in layouts: `aac_normal_chan_layouts` (`aacenctab.h:47-61`) has `4.0` (centre + pair + back centre).
    The PCE table (`aacenc.c:90-300`) adds `3.1`, `2.2` and `quad`. Any other layout, including an ambisonic
    one, fails with "Unsupported channel layout" (`:1171-1192`).
  - So label W,Y,Z,X as `4.0` (or `quad`). Check with a 4-tone test that channels come back in order.
  - Intensity stereo and PNS are on by default (`aac_is=1`, `aac_pns=1`, `:1283-1284`). Both are perceptual
    tricks that would smear ambisonic components, so set them to 0. M/S is reversible and can stay.
    **This is my reasoning, not tested.**
  - Bitrate: at least 256 kb/s, YouTube's minimum for spatial AAC. 320–384 kb/s is safer.

## Sources opened

- FFmpeg: github.com/FFmpeg/FFmpeg at n6.1.1, n7.1, n8.1.3, n9.0.2. Files: `libavcodec/{vaapi_encode.[ch],
  vaapi_encode_{h264,h265,av1}.c, hw_base_encode.[ch], vulkan_encode.[ch], libx264.c, libx265.c, libsvtav1.c,
  libopusenc.c, aacenc.c, aacenctab.h, h264_levels.c, h265_profile_level.c, av1_levels.c, avcodec.h, codec.h}`,
  `libavutil/{hwcontext.[ch], hwcontext_vaapi.c, hwcontext_vulkan.c, hwcontext_drm.c, frame.h}`,
  `libavformat/{movenc.c, mov.c}`, `Changelog`, `configure`.
- Mesa 26.2.5 via github.com/chaotic-cx/mesa-mirror (unofficial mirror). Files: `src/amd/common/ac_video.c`,
  `ac_gpu_info.c`, `src/gallium/drivers/radeonsi/mm/{si_mm_screen.c, radeon_vcn_enc.c, radeon_vcn_enc_4_0.c}`,
  `src/gallium/frontends/va/{config.c, encode.c, va_private.h}`,
  `src/amd/vulkan/{radv_video_enc.c, radv_video.c, radv_physical_device.c, radv_instance.c}`,
  `docs/envvars.rst`, `meson.options`.
- Linux github.com/torvalds/linux at v6.19, v7.0, v7.3: `drivers/gpu/drm/amd/amdgpu/soc21.c`, `amdgpu_vcn.c`.
- github.com/rpmfusion/ffmpeg (f44) `ffmpeg.spec`; github.com/rpmfusion/mesa-freeworld (f44)
  `mesa-freeworld.spec`.
- github.com/google/spatial-media: `spatialmedia/{__main__.py, gui.py, metadata_utils.py}`, `README.md`.
- https://packages.fedoraproject.org/pkgs/ffmpeg/libavcodec-free/fedora-44.html (and the fedora-43 page);
  https://packages.fedoraproject.org/pkgs/ffmpeg/ffmpeg-free/fedora-44.html
- https://support.google.com/youtube/answer/1722171, /answer/6178631?hl=en, /answer/6395969?hl=en,
  /answer/6396222?hl=en; https://vr.youtube.com/create/360/
- https://fftrac-bg.ffmpeg.org/wiki/Encode/AAC
- https://openbenchmarking.org/result/2501054-NE-9950XSVTA47 ,
  https://mail.openbenchmarking.org/result/2605017-NE-9950X3D2C40 ,
  https://mail.openbenchmarking.org/result/2408222-NE-M1446523427 , https://openbenchmarking.org/test/pts/x265 ,
  https://www.phoronix.com/review/ryzen-ai-max-395-9950x-9950x3d/7 ,
  https://www.phoronix.com/review/amd-ryzen-9950x-9900x/10
- Blocked from here: gitlab.freedesktop.org, archive.mesa3d.org, docs.mesa3d.org, src.fedoraproject.org
  (proxy 403 or Anubis), and ffmpeg.org.
