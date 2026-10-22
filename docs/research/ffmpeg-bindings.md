# FFmpeg bindings for Spherecut

Research date: 2026-10-22. Sources are listed at the end. The build results
come from test crates compiled in this workspace against Ubuntu's system
FFmpeg 6.1.1 and against minimal FFmpeg 7.1, 8.1 and 9.0.2 builds made from
the upstream git tags.

## Recommendation

**Use `ffmpeg-next` for demux, decode, encode, mux, swscale and swresample,
and call `ffmpeg-sys-next` directly for hardware acceleration, inside our own
small safe RAII layer in `spherecut-media`.** Link the system FFmpeg
dynamically through pkg-config. Support FFmpeg 6.1 through 9.x from one
source tree, using version cfgs set at compile time.

```toml
# crates/media/Cargo.toml (when the crate is created)
ffmpeg-next     = { version = "=9.0.0", default-features = false, features = ["codec", "format", "software-scaling", "software-resampling"] }
ffmpeg-sys-next = { version = "=9.0.1", default-features = false, features = ["avcodec", "avformat", "swscale", "swresample"] }
# add "filter" to both only if we use libavfilter (e.g. scale_vaapi, v360_vulkan)
```

Pin with `=`. ffmpeg-next's README says minor versions may break the API.
Move to `ffmpeg-next 9.0.1` once it is on crates.io: git master already has
version 9.0.1 (2026-10-09), with Spherical/Stereo3D wrappers and two
soundness fixes.

Why this choice:

1. **It builds on every target FFmpeg.** It compiled and ran here against
   6.1, 7.1, 8.1 and 9.0.2 with no feature flags, because the sys build
   script detects the version itself. Upstream CI tests FFmpeg 3.4 through
   9.0 in containers. rsmpeg fails to build on 9.0, which Arch and Fedora 45
   ship now.
2. **It binds the hwcontext structs we need.** Since 9.0.0, ffmpeg-sys-next
   binds `hwcontext_vaapi.h`, `hwcontext_vulkan.h` and `hwcontext_drm.h`
   (`AVVAAPIDeviceContext`, `AVVulkanDeviceContext`, `AVVkFrame`,
   `AVDRMFrameDescriptor`). It does this through stub headers, so no
   Vulkan SDK is needed at build time. Since 9.0.1 it also binds
   `spherical.h` and `stereo3d.h`. These structs are what later
   zero-copy work needs (VA-API → DRM PRIME → Vulkan import, or
   `AVVkFrame` → wgpu). ffmpeg-sys-the-third binds only `hwcontext.h` and
   `hwcontext_drm.h`. rusty_ffmpeg binds only `hwcontext.h`.
3. **It is maintained again.** There were 46 commits in 2026, FFmpeg 9.0
   support landed in July 2026, and the main maintainer is Adrian Eddy of
   Gyroflow. It has 8.1 M downloads, against 136 k for ffmpeg-the-third.
4. **Gyroflow is built on it.** Gyroflow is GPL-3.0-or-later and its
   `src/rendering/ffmpeg_hw.rs` (436 lines) does VA-API/Vulkan device
   creation, `get_format`, transfer-format negotiation and encoder
   `hw_frames_ctx` setup on top of `ffmpeg-next 9.0.0`. We may reuse it
   with attribution.
5. **No binding has a safe hwaccel API we would keep.** rsmpeg has the best
   one, but its other problems rule it out. The hardware path is a few
   hundred lines of raw FFI in any case. Writing it ourselves over
   ffmpeg-sys-next gives the same result as a bindgen layer of our own,
   without maintaining a 1,900-line build script for version detection.

Actions needed outside this document. None of them has been made yet.

* **cargo-deny will reject the build as configured.** ffmpeg-next,
  ffmpeg-sys-next and ffmpeg-the-third are licensed WTFPL, and `deny.toml`
  does not allow WTFPL. I ran the project's own `deny.toml` against test
  crates: the result was `licenses FAILED` (WTFPL rejected) for
  ffmpeg-next and ffmpeg-the-third, and `advisories FAILED` for rsmpeg
  (RUSTSEC-2024-0436, `paste` unmaintained). The FSF lists WTFPL as
  GPL-compatible. Add `"WTFPL"` to `allow`, preferably as a per-crate
  `[[licenses.exceptions]]` for `ffmpeg-next` and `ffmpeg-sys-next`, and
  record the reason in DECISIONS.md.
* Add a `build.rs` to `spherecut-media` that copies ffmpeg-next's 15-line
  `DEP_FFMPEG_*` → `cfg(feature = "ffmpeg_X_Y")` script. It should also
  fail with a clear message below FFmpeg 6.1, i.e. when `ffmpeg_6_1` is not
  set.

## 1. What the target distros ship

| Distro | Package (full / free) | FFmpeg | libavcodec | Notes |
|---|---|---|---|---|
| Ubuntu 24.04 (this workspace) | `libavcodec-dev` … | 6.1.1-3ubuntu5 | 60.31 | libavutil 58.29, swscale 7.5, swresample 4.12 |
| Fedora 42 | RPM Fusion `ffmpeg` 7.1.4 / `ffmpeg-free` 7.1.x | 7.1 | 61 | **EOL 2026-05-13**. Drop it. |
| Fedora 43 | RPM Fusion `ffmpeg` 7.1.5-1 / `ffmpeg-free` 7.1.5-1 | 7.1 | 61 | EOL about 2026-12-09 |
| Fedora 44 | RPM Fusion `ffmpeg` 8.1.3-1 / `ffmpeg-free` 8.1.3-1 (shipped 8.0.1) | 8.1 | 62 | current stable |
| Fedora 45 (GA scheduled 2026-10-30) | RPM Fusion `ffmpeg` 9.0.2-1 / `ffmpeg-free` 9.0.2-2 | 9.0 | 63 | also Rawhide (fc46) |
| Arch Linux `extra/ffmpeg` | `2:9.0.2-2` (updated 2026-10-03) | 9.0 | 63 | libavutil 61, swscale 10, swresample 7; license GPL-3.0-only |

So "8.x on Arch" in the brief is out of date: **Arch and Fedora 45 are on
9.0**. The only compat package in Arch extra is `ffmpeg4.4`.

Header packages:

* **Fedora, RPM Fusion:** `ffmpeg-devel`. It conflicts with
  `ffmpeg-free-devel`. Install with
  `sudo dnf swap ffmpeg-free ffmpeg --allowerasing && sudo dnf install ffmpeg-devel`.
* **Fedora, stock:** `ffmpeg-free-devel` is a metapackage. It pulls in
  `libavcodec-free-devel`, `libavformat-free-devel`, `libavutil-free-devel`,
  `libswscale-free-devel`, `libswresample-free-devel`,
  `libavfilter-free-devel` and `libavdevice-free-devel`. These have the
  same headers and sonames as the full build, so a binary built against
  them also runs on RPM Fusion's libs or on `libavcodec-freeworld`.
* **Arch:** headers ship in `ffmpeg` itself.
* **Ubuntu:** `libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev`.
* **Build tools:** `pkg-config` and libclang for bindgen. That is
  `clang-devel` (or `clang-libs`) on Fedora, `clang` on Arch, and
  `libclang-dev` on Ubuntu.

Hardware support in the distro builds:

* RPM Fusion builds with libva and Vulkan
  (`--enable-vulkan --enable-libshaderc`, Vulkan headers ≥ 1.3.277) and is
  GPLv3+.
* Arch's build depends on libva, vulkan-icd-loader and libplacebo.
* Fedora's `ffmpeg-free` has H.264/HEVC compiled out. Fedora's Mesa also
  disables the H.264/HEVC VA-API entry points. On Fedora, hardware HEVC
  needs RPM Fusion `ffmpeg` (or `libavcodec-freeworld`) plus
  `mesa-va-drivers-freeworld`.
* *Not checked here:* whether Fedora's Mesa build options also strip
  H.264/H.265 from RADV/ANV Vulkan Video. Verify on odin with `vulkaninfo`.

## 2. The candidates

| | **ffmpeg-next** + ffmpeg-sys-next | **ffmpeg-the-third** + sys | **rsmpeg** + rusty_ffmpeg | own bindgen layer |
|---|---|---|---|---|
| Latest | 9.0.0 (2026-08-05); sys 9.0.1 (2026-10-09); 9.0.1 in git | 6.0.0+ffmpeg-9.0 (2026-08-09) | rsmpeg 0.18.0+ffmpeg.8.0 (2025-08-24); rusty_ffmpeg 0.17.0+ffmpeg.8.1 (2026-04-10) | n/a |
| Activity | 46 commits in 2026 (A. Eddy, Z. Wang + contributors) | 25 commits in 2026 (FreezyLemon, J. Holmer) | **no commits since 2025-08-24**; PRs from 2026-05 to 2026-10 unmerged | ours |
| FFmpeg range | 3.4–9.0, autodetected; CI 3.4…9.0 | 5.1–9.0, autodetected; CI 5.1…9.0 | 6.x–8.x, chosen with a **Cargo feature** (`ffmpeg6`/`ffmpeg7`/`ffmpeg7_1`/`ffmpeg8`, default `ffmpeg8`); no 9.0 | whatever we write |
| Built here on 6.1 / 7.1 / 8.1 / 9.0.2 | ✅ / ✅ / ✅ / ✅ | ✅ / ✅ / ✅ / ✅ | ✅ / ✅ / ✅ / **❌** (6 errors: `AVCodec.pix_fmts` etc. removed). With the wrong feature (default `ffmpeg8` on 6.1) it also fails. | — |
| Linking | pkg-config, dynamic by default (`FFMPEG_DIR` override; optional `static`/`build`) | pkg-config, dynamic | pkg-config only with feature `link_system_ffmpeg`; otherwise `FFMPEG_PKG_CONFIG_PATH` / `FFMPEG_DLL_PATH` env | pkg-config |
| License | WTFPL (GPL-compatible; **not in our deny.toml**) | WTFPL (same) | MIT | GPL-3.0-or-later |
| Safe hwaccel API | none (pixel formats only); raw `ffi::*` + `as_mut_ptr()` hatches | none | `AVHWDeviceContext::create/create_derived`, `AVHWFramesContext` alloc/init/get_buffer, `set_hw_device_ctx`, `set_hw_frames_ctx`, `get_format` setter, `AVFrame::hwframe_transfer_data` | n/a |
| Per-API hwcontext structs in sys | **vaapi, vulkan, drm, cuda, qsv, …** (stub headers); spherical/stereo3d (9.0.1) | drm only (plus stereo3d) | none (only `hwcontext.h`) | any |
| Bindgen / clang | bindgen 0.73 (runtime libclang), `cc` | bindgen 0.72 + `clang` crate (runtime libclang), `cc` | bindgen 0.71 (0.72 in rusty_ffmpeg 0.17); optional prebuilt binding (8.1 only) | bindgen |
| Deps (normal+build) | 26 crates | 29 (duplicate `shlex`) | 41 (bon, darling, paste, two `syn`, two `prettyplease`) | ~20 |
| cargo-deny (project config) | licenses FAILED (WTFPL) | licenses FAILED (WTFPL) | **advisories FAILED** (RUSTSEC-2024-0436 `paste`) | ok |
| Clean debug build, 4 cores | 37 s | 39 s | 35 s | — |
| MSRV / edition | edition 2024 (needs ≥1.85; we are 1.88) | 1.80 | 1.81 | — |

Other notes:

* **ffmpeg-next** changes its public API with the detected FFmpeg version.
  `ChannelLayout` is a `u64` bitflags type before 7.0 and a wrapper around
  `AVChannelLayout` from 7.0. The bitflags type cannot express ambisonic
  order, so on 6.1 we must read `ch_layout` (present since 5.1) through
  `ffi` for ambiX. Our code needs the same cfgs where it touches those
  types. The wrapper handles the 7.0/8.0 removals internally: the
  `avcodec_close` drop is cfg'd, `is_key` uses `AV_FRAME_FLAG_KEY` on
  8.0+, and `frame.duration` is used on 7.0+. Its README still says
  "maintenance mode" and declines API-improvement PRs, but the 2026 history
  shows feature PRs merged (custom IO, interrupts, codec setters,
  Spherical). The bus factor is about two people.
* **ffmpeg-the-third** has the cleaner and more stable API: a uniform
  5.1+ surface, newtype enums that avoid UB on unknown values, and a full
  `ChannelLayout`/`ChannelOrder` API including Ambisonic. It is the
  fallback if ffmpeg-next stalls. The fork shares the same lineage, so
  porting is moderate work. It was not chosen because it lacks the
  vaapi/vulkan hwcontext bindings (we would have to bindgen those
  ourselves) and is less widely used.
* **rsmpeg** is closest to "safe FFmpeg C API" and has the best hwaccel
  coverage. It is rejected because it is stale, does not support FFmpeg
  9, needs a per-distro Cargo feature, fails our cargo-deny advisories
  check and has a heavier dependency tree.
* **An own bindgen layer** gives full control, but we would rebuild what
  ffmpeg-sys-next already does: pkg-config probing, the version ladder
  (`ffmpeg_6_1` = lavc 60.31 … `ffmpeg_9_0` = lavc 63.1), detection of
  about 100 `FF_API_*` macros, blocklists and the hwcontext stubs. Not
  worth it. Our safe RAII layer still lives in `spherecut-media`.
* **Others seen on crates.io and not evaluated in depth:** `ac-ffmpeg`
  0.19.0 (MIT, 2025-05) and `scuffle-ffmpeg` 0.3.5 (2025-06). Neither has
  had a release since mid-2025. `ez-ffmpeg` 0.19.3 is a high-level
  CLI-style wrapper.

## 3. Handling the version spread (6.1 → 9.0)

**Policy:** one source tree, compile-time cfgs, dynamic linking. A binary is
tied to the FFmpeg major it was built against, because the sonames differ:
lavc 60/61/62/63. Distro packages (RPM spec, PKGBUILD) are therefore
rebuilt per release, as Arch's "ffmpeg 9.0 rebuild" todo list does for
about 105 packages.

* **Floor: FFmpeg 6.1** (lavc 60.31, lavu 58.29), the oldest version we
  need (this workspace). It already has every replacement API listed below
  except `avcodec_get_supported_config` and the Vulkan `qf[]` queue API.
* **Gate the Vulkan hwaccel path to ≥ 7.1**, and prefer 8.1+. Vulkan
  decode only arrived in 6.1. The `AVVulkanDeviceContext` queue API
  changed in 7.1, 8.1 and 9.0, and sharing wgpu's `VkDevice` with FFmpeg
  means filling that struct ourselves. VA-API, `hw_device_ctx`,
  `hw_frames_ctx`, `av_hwframe_transfer_data` and `av_hwframe_map` have
  been stable throughout the range.
* **CI matrix: 6.1** (this workspace), **7.1** (F43), **8.1** (F44),
  **9.0** (Arch, F45). Minimal shared builds from the upstream tags
  (`--disable-everything --enable-vaapi` plus the few components we test)
  took about 1.5 min each here on 4 cores. A `cargo xtask` job can build
  them into prefixes and run `cargo check` plus Tier 1 tests per prefix
  with `PKG_CONFIG_PATH`/`LD_LIBRARY_PATH`. Vulkan hwcontext headers need
  Vulkan headers ≥ 1.3.277, which this workspace lacks; Ubuntu's own 6.1
  build ships `hwcontext_vulkan.h`.

API breaks that matter to us, verified against the release tags and
`doc/APIchanges`:

| Change | Deprecated | Removed | Use instead (available since) |
|---|---|---|---|
| Old channel-layout API (`AVCodecContext/AVFrame.channels`, `.channel_layout` u64) | 5.1 | **7.0** | `ch_layout: AVChannelLayout` (5.1); ambiX = `AV_CHANNEL_ORDER_AMBISONIC` |
| `AVFrame.pkt_duration` | 5.1 | **7.0** | `AVFrame.duration` (5.1) |
| `AVCodecContext.frame_number` | 6.0 | **7.0** | `frame_num` (6.0) |
| `avcodec_close()` | 7.0 | **8.0** | `avcodec_free_context()` only |
| `AVCodecContext.ticks_per_frame` | 6.1 | **8.0** | `framerate` for encoding, `AV_CODEC_PROP_FIELDS` for decoding; timestamps from `pts` + `duration` in the stream time base |
| `AVFrame.key_frame`, `.interlaced_frame`, `.top_field_first` | 6.1 | **8.0** | `AVFrame.flags` & `AV_FRAME_FLAG_KEY` / `_INTERLACED` / `_TOP_FIELD_FIRST` (6.1, lavu 58.7) |
| `AVFrame.pkt_pos`, `.pkt_size` | 6.1 | **8.0** | none; track on the packet side |
| `AVStream.side_data` | 6.1 | **8.0** | `AVCodecParameters.coded_side_data` (6.1); spherical/stereo3d side data lives there |
| `FF_PROFILE_*` | 6.1 | **8.0** | `AV_PROFILE_*` (6.1), e.g. `AV_PROFILE_HEVC_MAIN_10` for doctor checks |
| `AVCodec.pix_fmts`, `.sample_fmts`, `.supported_framerates`, `.supported_samplerates`, `.ch_layouts` | 7.1 | **9.0** | `avcodec_get_supported_config()` (7.1); cfg fallback for 6.1/7.0 |
| `AVCodecContext.properties` | 7.1 | **9.0** | codec descriptor `props` |
| `AVVulkanDeviceContext.queue_family_index`, `nb_graphics_queues`, `queue_family_tx_index`, … | 7.1 | **9.0** | `qf[]` / `nb_qf` (7.1); `lock_queue`/`unlock_queue` deprecated 8.1; `AVVkFrame.access` became `VkAccessFlagBits2` in 9.0 |
| swscale | — | — | `sws_getContext`/`sws_scale` still present in 9.0. `SwsContext` public plus `sws_frame_setup` (7.1); Vulkan swscale (8.1). Plain ffmpeg-next scaling works on every version |

Relevant additions along the way:

* 6.1: Vulkan decode for H.264/HEVC/AV1.
* 7.1: Vulkan H.264/H.265 encode.
* 8.0: AV1 Vulkan encode, VP9 Vulkan decode.
* 9.0: a `v360_vulkan` filter, which is interesting as a reference for
  equirect math.

Rules for our raw-FFI code:

* Use only the "instead" column above.
* Wrap the 7.1+ and 9.0-only pieces in `#[cfg(feature = "ffmpeg_7_1")]` and
  similar.
* Never read struct fields that the table marks as removed.

## Sources

Commands and crate data:

* crates.io API for each crate's versions, dates, licenses and downloads:
  `https://crates.io/api/v1/crates/{rsmpeg,rusty_ffmpeg,ffmpeg-next,ffmpeg-sys-next,ffmpeg-the-third,ffmpeg-sys-the-third}`.
  `cargo search ffmpeg`, `cargo info ffmpeg-next`.

Binding repositories (git history, manifests, build scripts, CI):

* https://github.com/zmwangx/rust-ffmpeg and https://github.com/zmwangx/rust-ffmpeg-sys
  (`hwcontext_wrapper.h`, `hwcontext_stubs/`, `build.rs` version table,
  `.github/workflows/build.yml`).
* https://github.com/shssoichiro/ffmpeg-the-third (CHANGELOG, CI matrix,
  sys `build.rs` header list).
* https://github.com/larksuite/rsmpeg and https://github.com/CCExtractor/rusty_ffmpeg
  (feature flags, `src/avutil/hwcontext.rs`, PR refs 253–257).
* Gyroflow: https://github.com/gyroflow/gyroflow (`Cargo.toml`: `ffmpeg-next = "9.0.0"`;
  `src/rendering/ffmpeg_hw.rs`, `zero_copy.rs`, GPL-3.0-or-later).

FFmpeg:

* https://github.com/FFmpeg/FFmpeg tags n6.1 (2023-11-10), n7.0, n7.1
  (2024-09-29), n8.0 (2025-08-22), n8.1 (2026-03-16), n9.0 (2026-08-03),
  n9.0.2 (2026-09-18).
* Per-library `version.h` / `version_major.h`, `doc/APIchanges`,
  `Changelog`.

Distributions:

* RPM Fusion: https://github.com/rpmfusion/ffmpeg branches f42/f43/f44/f45
  (`ffmpeg.spec` Version, %changelog, configure flags, `ffmpeg-devel`).
* Fedora: https://packages.fedoraproject.org/pkgs/ffmpeg/ffmpeg-free-devel,
  …/ffmpeg-free-devel/fedora-44-updates.html,
  …/ffmpeg-free-devel/fedora-44.html,
  …/libavformat-free/fedora-44-updates.html.
* Arch: https://archlinux.org/packages/extra/x86_64/ffmpeg/ (2:9.0.2-2) and
  https://archlinux.org/todo/ffmpeg-90-rebuild/.
* Fedora lifecycle: https://www.lansweeper.com/blog/eol/fedora-linux-end-of-life/
  (F42 EOL 2026-05-13, F43 EOL 2026-12-09) and
  https://www.warp2search.net/story/fedora-linux-45-rc11-available-for-testing-ga-set-for-oct-30/
  (F45 GA 2026-10-30).
* Fedora codecs: https://fedoraproject.org/wiki/Hardware_Video_Acceleration and
  https://computingforgeeks.com/rpm-fusion-fedora-44-codecs/ (ffmpeg-free
  lacks H.264/HEVC; `dnf swap`; freeworld packages).

Local checks:

* Test crates built against system 6.1 and against `--prefix` builds of
  n7.1 / n8.1 / n9.0.2.
* `cargo deny check` with `/home/claude/spherecut/deny.toml`.
* `cargo tree -e normal,build`.
