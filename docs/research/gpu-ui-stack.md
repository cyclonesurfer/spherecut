# GPU / UI stack: versions, compatibility, and API notes

Research date: 2026-10-22. Toolchain: Rust 1.97.0 stable.
Sources: the crates.io API (versions, MSRV, licenses, dependency requirements),
upstream changelogs and source (egui `main`, wgpu `main`, egui_dock and
rerun-io/egui_crates git, plus the published crate sources), and builds and
tests run on lavapipe (Mesa 25.2.8, LLVM 20.1.2, Vulkan 1.4.318). The test
code was a throwaway crate outside this repository. "Verified" below means it
compiled and ran there; "expected" means it was worked out from wgpu-hal's
detection code and driver capabilities but not run on that hardware.

---

## TL;DR

* **Use egui 0.37.0 and wgpu 30.0.1.** eframe 0.37.0, egui-wgpu 0.37.0 and
  egui_kittest 0.37.0 all require `wgpu ^30.0` and `winit ^0.30.13`. egui
  0.36.x also used wgpu 30, so choosing between the two egui lines doesn't
  change the GPU side.
* **Docking: egui_tiles 0.18.0.** egui_dock 0.21.1 requires `egui ^0.36`.
  Its port to 0.37 is open PR #353 (2026-10-20), which builds against 0.37
  (I checked), but it isn't on crates.io. Our `deny.toml` has
  `unknown-git = "deny"`, so we can't use a git pin without an exception.
  egui_tiles comes from the egui authors' company (Rerun), ships for each
  egui release within days, and has no floating/undocked windows. If we
  need floating windows, switch to egui_dock 0.22 when it's published.
* **Math: glam 0.34.1** (`Quat` and `DQuat`, shortest-path `slerp` plus
  `slerp_long`, `bytemuck`, `serde`). Not nalgebra.
* **Time: keep our own `Rational`** (already in `spherecut_core::time`).
  Don't add num-rational.
* **MSRV:** the egui family requires **Rust 1.95**. The workspace currently
  says `rust-version = "1.88"`, so raise it to 1.95 when `spherecut-app`
  and `spherecut-render` land (or set 1.95 on those crates only).
* **Headless:** wgpu 30 on lavapipe works. egui_kittest snapshots with a
  **custom `CallbackTrait` paint callback** render deterministically on
  lavapipe (3/3 identical re-runs). NV12 multi-planar, R8+RG8 planes and
  P010-style R16+RG16 planes all convert to the right RGB.
* **YUV path:** upload decoded frames as **separate single-plane textures**
  (R8Unorm + Rg8Unorm, or R16Unorm + Rg16Unorm) and convert in WGSL. That is
  what ARCHITECTURE.md already plans. It is filterable and mip-able, it runs
  on lavapipe, and it matches the only DMA-buf import wgpu 30 has
  (single-plane).

---

## 1. Version matrix (latest stable on crates.io as of 2026-10-22)

| Crate | Pin | Released | MSRV | License | Notes |
|---|---|---|---|---|---|
| wgpu | **30.0.1** | 2026-08-22 | 1.87 | MIT OR Apache-2.0 | Policy: MSRV never above stable−3. 31 is on `main` (breaking, see §5). |
| naga | **30.0.1** | 2026-08-22 | 1.87 | MIT OR Apache-2.0 | Must be the same major as wgpu. wgpu 30 also pulls `naga-types` 30 and `wgpu-naga-bridge` 30. |
| eframe | **0.37.0** | 2026-10-15 | 1.95 | MIT OR Apache-2.0 | `egui-wgpu ^0.37`, `winit ^0.30.13`, `image ^0.25.10` (png), `ron ^0.12.2` (persistence). |
| egui | **0.37.0** | 2026-10-15 | 1.95 | MIT OR Apache-2.0 | `default_fonts` is **no longer a default feature** of `egui`. |
| egui-wgpu | **0.37.0** | 2026-10-15 | 1.95 | MIT OR Apache-2.0 | `wgpu ^30.0` (features `std`, `wgsl`). |
| egui-winit | 0.37.0 (transitive) | 2026-10-15 | 1.95 | MIT OR Apache-2.0 | `winit ^0.30.13`, `accesskit_winit ^0.34`. |
| egui_kittest | **0.37.0** | 2026-10-15 | 1.95 | MIT OR Apache-2.0 | `kittest ^0.5.0` (MSRV 1.95); its wgpu dep turns on `vulkan`, `gles`, `metal`, `dx12`. |
| egui_tiles | **0.18.0** | 2026-10-19 | 1.95 | MIT OR Apache-2.0 | `egui ^0.37.0`. Repo: rerun-io/egui_crates. |
| egui_dock | 0.21.1 | 2026-08-06 | 1.95 | MIT | **`egui ^0.36` only.** Repo moved to anhosh/egui_dock. 0.37 port is PR #353 (`23f0fcf9`). |
| egui_plot | **0.38.0** (optional) | 2026-10-19 | 1.95 | MIT OR Apache-2.0 | `egui ^0.37.0`. Its numbering is one ahead of egui (0.37 → egui 0.36). |
| winit | 0.30.13 (via eframe) | 2026-03-02 | 1.70 | Apache-2.0 | 0.31.0-beta.3 (2026-09-04) exists. egui has only a WIP branch `lucas/winit-0.31`. |
| glam | **0.34.1** | 2026-10-06 | 1.85 | MIT OR Apache-2.0 | 0.34 removed the deprecated `look_at_*`/`perspective_*` (use `glam::camera` / `glam::dcamera`). |
| nalgebra | 0.35.0 (not chosen) | 2026-05-24 | 1.89 | Apache-2.0 | Its `glam` interop only covers glam ≤ 0.33. |
| proptest | **1.12.0** | 2026-10-10 | 1.88 | MIT OR Apache-2.0 | |
| criterion | **0.8.2** | 2026-02-04 | 1.86 | Apache-2.0 OR MIT | Repo moved to `criterion-rs/criterion.rs`. |
| thiserror | **2.0.21** | 2026-09-23 | 1.77 | MIT OR Apache-2.0 | |
| anyhow | **1.0.104** | 2026-07-18 | 1.68 | MIT OR Apache-2.0 | |
| tracing | **0.1.44** | 2025-12-18 | 1.65 | MIT | |
| tracing-subscriber | **0.3.23** | 2026-03-13 | 1.65 | MIT | |
| serde | **1.0.229** | 2026-07-18 | 1.56 | MIT OR Apache-2.0 | |
| ron | **0.12.2** | 2026-06-22 | 1.64 | MIT OR Apache-2.0 | Same line eframe's `persistence` uses. |
| num-rational | 0.4.2 (not chosen) | 2024-05-08 | 1.60 | MIT OR Apache-2.0 | crates.io "newest" shows 0.1.43, a maintenance release of the old 0.1 line. |
| cpal | **0.18.2** | 2026-08-16 | 1.85 | Apache-2.0 | New native PipeWire and PulseAudio hosts (see §8). |
| image | **0.25.10** (`png` only) | 2026-03-10 | 1.88 | MIT OR Apache-2.0 | Same version eframe requires. |
| clap | **4.6.7** | 2026-09-14 | 1.85 | MIT OR Apache-2.0 | |
| bytemuck | 1.25.2 | 2026-07-19 | – | Zlib OR Apache-2.0 OR MIT | For GPU uniform structs. |
| pollster | 1.0.1 | 2026-07-10 | 1.69 | Apache-2.0/MIT | Same major as eframe/kittest. 0.4 would duplicate it. |

The highest MSRV in the set is **1.95**, from the egui family, egui_tiles and kittest.

### Hard compatibility constraints

* **egui crates move in lock-step.** egui, eframe, egui-wgpu, egui-winit,
  egui_kittest, epaint, ecolor and emath are always the same version and
  each requires `^0.37.0` of the others.
* **The wgpu major is set by egui-wgpu.** eframe 0.37 → egui-wgpu 0.37 →
  `wgpu ^30.0`. Two wgpu majors in one graph can't share a `Device`, so
  **don't move to wgpu 31 until egui-wgpu does.** Keep `naga` on that same
  major.
* **winit is set by eframe** (`^0.30.13`). Add winit as a direct dependency
  only to turn on features (§3), never to change its version.
* **Docking crates track egui minors.** egui_tiles 0.18.x needs egui 0.37;
  egui_tiles 0.17.x and egui_dock 0.21.x need egui 0.36.
* **egui_plot 0.38 needs egui 0.37.**
* **egui_kittest 0.37 needs the same egui 0.37.** Its `wgpu` feature brings
  in the GL backend for test builds only.

How quickly each docking crate followed an egui release:

| egui | egui_tiles | egui_dock |
|---|---|---|
| 0.33.0 (2025-10-09) | 0.14.0 (+1 day) | 0.18.0 (+22 days) |
| 0.34.0 (2026-03-26) | 0.15.0 (+1 day) | 0.19.0 (+3 days) |
| 0.35.0 (2026-06-25) | 0.16.0 (+1 day) | 0.20.0 (+2 days) |
| 0.36.0 (2026-08-05) | 0.17.0 (same day) | 0.21.0 (same day) |
| 0.37.0 (2026-10-15) | 0.18.0 (+4 days) | not yet (PR open, +7 days so far) |

---

## 2. Recommended `[workspace.dependencies]`

I resolved, built and tested this exact block (every entry except cpal) in
a scratch workspace with `resolver = "3"` and `rust-version = "1.95"`. All
headless and kittest tests passed. cpal resolved with
`pipewire` + `pulseaudio` alongside eframe, but I couldn't compile it here
because the machine has no ALSA headers (§8). Items marked "existing" are
already in our Cargo.toml; I only raised their minimum versions to the
current releases (all semver-compatible).

```toml
[workspace.package]
# egui 0.37 / egui_tiles 0.18 / egui_kittest 0.37 require 1.95.
# (Alternatively keep 1.88 here and set rust-version = "1.95" on app/render only.)
rust-version = "1.95"

[workspace.dependencies]
# --- GPU -------------------------------------------------------------------
# Vulkan-only in shipped builds. wgpu's major must match egui-wgpu's.
wgpu = { version = "30.0.1", default-features = false, features = ["std", "parking_lot", "vulkan", "wgsl"] }
naga = { version = "30.0.1", features = ["wgsl-in"] }   # dev-only: offline WGSL validation in unit tests
bytemuck = { version = "1.25.2", features = ["derive"] }
pollster = "1.0.1"

# --- UI --------------------------------------------------------------------
# `wgpu_no_default_features` keeps egui-wgpu from turning on wgpu's default
# features (GL/EGL backend etc.); our `wgpu` entry above picks the backends.
eframe = { version = "0.37.0", default-features = false, features = [
    "wgpu_no_default_features", "wayland", "x11", "accesskit", "default_fonts", "persistence",
] }
egui = "0.37.0"
egui-wgpu = { version = "0.37.0", default-features = false }   # eframe already turns on `winit` + `capture`
egui_tiles = "0.18.0"
egui_plot = "0.38.0"                                            # optional (telemetry/gyro graphs)
# Not a version choice: turns back on the winit features eframe's defaults would
# have given us (runtime-dlopen libwayland, plus an Adwaita CSD frame on GNOME, which has no SSD).
winit = { version = "0.30.13", default-features = false, features = [
    "rwh_06", "wayland", "wayland-dlopen", "wayland-csd-adwaita", "x11",
] }

# --- Testing ---------------------------------------------------------------
egui_kittest = { version = "0.37.0", features = ["wgpu", "snapshot"] }
proptest = "1.12.0"                                  # existing ("1.8")
criterion = { version = "0.8.2", features = ["html_reports"] }

# --- Math / core -----------------------------------------------------------
glam = { version = "0.34.1", features = ["serde", "bytemuck"] }   # existing ("0.34", serde)
thiserror = "2.0.21"                                 # existing ("2.0")
anyhow = "1.0.104"                                   # existing
tracing = "0.1.44"                                   # existing
tracing-subscriber = { version = "0.3.23", features = ["env-filter"] }   # existing
serde = { version = "1.0.229", features = ["derive"] }                   # existing
ron = "0.12.2"                                       # existing ("0.12")
clap = { version = "4.6.7", features = ["derive"] }  # existing ("4.5")

# --- Media / IO ------------------------------------------------------------
# ALSA is always compiled in (libasound2-dev). `pipewire` needs libpipewire-0.3-dev + libclang (bindgen).
# `pulseaudio` is a pure-Rust protocol client (works through pipewire-pulse).
# At runtime the default host is the first one available: PipeWire > PulseAudio > ALSA.
cpal = { version = "0.18.2", features = ["pipewire", "pulseaudio"] }
image = { version = "0.25.10", default-features = false, features = ["png"] }
```

Notes:

* **Fonts.** Keep `default_fonts` on eframe, or install fonts with
  `FontDefinitions` before the first frame. Any crate that runs kittest
  without depending on eframe (e.g. a UI widgets crate) needs
  `egui = { workspace = true, features = ["default_fonts"] }` in
  `[dev-dependencies]`. Without it, egui 0.37 + kittest 0.37 **panic**. I
  hit this: `No glyph for "T" (U+0054) in Proportional. Installed fonts: []`.
* **No GL backend in shipped builds.** egui-wgpu's default
  `WgpuSetupCreateNew` asks for `PRIMARY | GL` backends. With the block
  above, GL isn't compiled into non-dev builds (checked with
  `cargo tree -e features -i wgpu-hal`: `vulkan` only). Set
  `instance_descriptor.backends = wgpu::Backends::VULKAN` explicitly anyway.
* **Things I left out on purpose:**
  * eframe's `links` (`webbrowser`).
  * `system_fonts` (fontique), which made egui 0.37 able to render any
    system glyph. Turn it on if we need non-Latin text or colour emoji.
  * `glow`.
  * winit's `wayland-csd-adwaita-crossfont`.
* **egui_dock instead of egui_tiles:**
  * Once egui_dock 0.22 is published: `egui_dock = { version = "0.22", features = ["serde"] }`.
  * Before then: either use egui 0.36.2 for the whole family (`eframe`,
    `egui`, `egui-wgpu`, `egui_kittest` = 0.36.2; `egui_dock` 0.21.1;
    `egui_plot` 0.37.0), which also resolves and builds on wgpu 30.0.1,
    or pin `egui_dock = { git = "https://github.com/anhosh/egui_dock", rev = "23f0fcf9b1c2408d08006af3312c85af4ead3f90" }`.
    The git pin needs a `[sources] allow-git` entry in deny.toml.

### cargo-deny implications (current `deny.toml`)

I tallied the licenses in the resolved Linux graph. Everything is
GPL-3.0-compatible and already allowed, except:

* `epaint_default_fonts`:
  `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`. This is font data
  bundled into the binary. **We need a decision** (DECISIONS.md) and a
  crate-scoped exception
  `{ crate = "epaint_default_fonts", allow = ["OFL-1.1", "Ubuntu-font-1.0"] }`.
  The alternative is to turn off `default_fonts` and ship one OFL font
  ourselves via `FontDefinitions` (snapshot tests still need a bundled
  font for determinism).
* `colored` (MPL-2.0, already allowed) comes in only through `dify`, i.e.
  the kittest snapshot dev-dependency.
* `self_cell` is `Apache-2.0 OR GPL-2.0-only`; the Apache side is fine.

Expect `multiple-versions` warnings, e.g. thiserror 1/2, rustix 0.38/1,
hashbrown 0.16/0.17, itertools 0.13/0.15, getrandom 0.3/0.4. They come from
upstream and are harmless.

---

## 3. Custom wgpu paint callback in egui-wgpu 0.37 (Q1)

What the API looks like (`egui_wgpu::CallbackTrait`, unchanged in shape
since ~0.24; the render pass is `RenderPass<'static>`):

```rust
pub trait CallbackTrait: Send + Sync {
    fn prepare(&self, device: &wgpu::Device, queue: &wgpu::Queue,
               screen_descriptor: &egui_wgpu::ScreenDescriptor,   // size_in_pixels, pixels_per_point
               egui_encoder: &mut wgpu::CommandEncoder,
               callback_resources: &mut egui_wgpu::CallbackResources) -> Vec<wgpu::CommandBuffer> { Vec::new() }
    fn finish_prepare(&self, device: &wgpu::Device, queue: &wgpu::Queue,
                      egui_encoder: &mut wgpu::CommandEncoder,
                      callback_resources: &mut egui_wgpu::CallbackResources) -> Vec<wgpu::CommandBuffer> { Vec::new() }
    fn paint(&self, info: egui::PaintCallbackInfo,             // viewport, clip_rect, pixels_per_point, screen_size_px
             render_pass: &mut wgpu::RenderPass<'static>,
             callback_resources: &egui_wgpu::CallbackResources);
}
// CallbackResources = type_map::concurrent::TypeMap (insert/get by type).
// Emit:  ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, MyCallback { .. }));
```

How it behaves:

* **Order.** egui calls `prepare` for every callback, then `finish_prepare`
  for every callback, then `paint` inside egui's single render pass. Do
  heavy GPU work in `prepare`: compute or render into our own textures,
  using egui's encoder or returning our own `CommandBuffer`s. eframe submits
  egui's buffer and ours in one `queue.submit`. `paint` must only record
  draws into egui's pass.
* **Viewport and scissor.** Before `paint`, egui-wgpu sets the viewport to
  the callback `rect` (in physical pixels) and the scissor to the clip rect,
  then restores the full-screen scissor afterwards. So clip-space
  (−1..1) covers the callback rect, and the pipeline's colour target must be
  `render_state.target_format`.
* **Getting Device/Queue in eframe.** Both are verified to compile against
  eframe 0.37 with Vulkan-only setup and a custom adapter selector:
  ```rust
  // at creation:
  fn new(cc: &eframe::CreationContext<'_>) -> Self {
      let rs: &egui_wgpu::RenderState = cc.wgpu_render_state.as_ref().unwrap();
      // rs.device, rs.queue, rs.adapter, rs.instance, rs.target_format, rs.available_adapters
      rs.renderer.write().callback_resources.insert(ViewerResources::new(&rs.device, rs.target_format));
      ..
  }
  // every frame (eframe 0.37 App: fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)
  //                            plus optional fn logic(&mut self, ctx, frame)):
  if let Some(rs) = frame.wgpu_render_state() { /* rs.device / rs.queue */ }
  ```
* **Configuring device creation.**
  `NativeOptions { renderer: eframe::Renderer::Wgpu, wgpu_options: egui_wgpu::WgpuConfiguration { wgpu_setup, .. }, .. }`,
  where `wgpu_setup` is an `egui_wgpu::WgpuSetupCreateNew`. Its fields:
  * `instance_descriptor`: set `.backends`.
  * `native_adapter_selector`:
    `Arc<dyn Fn(&[wgpu::Adapter], Option<&wgpu::Surface>) -> Result<wgpu::Adapter, String>>`.
  * `device_descriptor`: `Arc<dyn Fn(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static>>`.
    Request `TEXTURE_FORMAT_16BIT_NORM` here (masked by `adapter.features()`).
  * Use `WgpuSetup::Existing(WgpuSetupExisting { instance, adapter, device, queue })`
    to share a device we created ourselves. This is the hook for sharing a
    `VkDevice` with FFmpeg's Vulkan hwcontext later.
  * egui-wgpu does **not** honour `WGPU_ADAPTER_NAME`; it honours
    `WGPU_BACKEND`, `WGPU_POWER_PREF` and the `WGPU_*` instance flags.
    Implement `WGPU_ADAPTER_NAME` in our selector, or use
    `wgpu::util::initialize_adapter_from_env` in the headless path.
* **Callback or texture?** An alternative is to render the viewer into our
  own texture in `prepare`, or on a worker, and show it with
  `renderer.register_native_texture(device, &view, FilterMode)` →
  `egui::TextureId` → `ui.image(..)`, updating it with
  `update_egui_texture_from_wgpu_texture`. Pick one or the other for the
  viewer. The callback avoids a copy and the extra texture. The texture
  route lets the export path and the GUI share the exact same render-target
  code. Both work with kittest.
* **Verified in kittest.** A `CallbackTrait` that draws an orange triangle
  rendered in an egui_kittest snapshot on lavapipe, with the pixel at the
  rect centre `[255, 102, 0, 255]`.

---

## 4. Headless wgpu on lavapipe and API changes up to wgpu 30 (Q2)

Verified on lavapipe:

```rust
let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env(); // v29+: no Default impl
desc.backends = wgpu::Backends::VULKAN;
let instance = wgpu::Instance::new(desc);                                        // v29+: BY VALUE

// A) wgpu's own fallback-adapter filter (keeps only DeviceType::Cpu adapters):
let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
    power_preference: wgpu::PowerPreference::None,
    force_fallback_adapter: true,
    compatible_surface: None,
    apply_limit_buckets: false,                     // v30 field; `..Default::default()` also works
}))?;                                               // v25+: Result<Adapter, RequestAdapterError>

// B) Pick by name or type (async since v28):
let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
    .into_iter()
    .find(|a| { let i = a.get_info(); i.device_type == wgpu::DeviceType::Cpu || i.name.contains("llvmpipe") })
    .expect("lavapipe");

let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
    label: Some("spherecut-headless"),
    required_features: wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
    required_limits: wgpu::Limits::default(),
    experimental_features: wgpu::ExperimentalFeatures::disabled(), // v27+
    memory_hints: wgpu::MemoryHints::Performance,
    trace: wgpu::Trace::Off,                                       // v25+: was a request_device() argument
}))?;
// Readback:  map_async(..)  →  device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(10s) })?
//            →  slice.get_mapped_range()?   // v30: returns Result
```

What lavapipe reported here: `name = "llvmpipe (LLVM 20.1.2, 256 bits)"`,
`driver = "llvmpipe"`, `device_type = Cpu`, `max_texture_dimension_2d = 16384`.
Option A returned that adapter.

**Pinning lavapipe in CI:**

* Install `mesa-vulkan-drivers`.
* Set `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json`
  (`VK_ICD_FILENAMES` on loaders older than 1.3.234). That way a host GPU
  can never be picked and change the golden images.
* Set `WGPU_BACKEND=vulkan`.
* Also assert `adapter.get_info().device_type == Cpu` in the harness.
* Record `driver_info` (the Mesa version) next to golden images; lavapipe
  output can change across Mesa/LLVM versions.

**Breaking changes in instance, adapter and device creation since wgpu 24**

| Version | Change |
|---|---|
| 24 | `Instance::new(&InstanceDescriptor)` (by reference). |
| 25 | `request_adapter` returns `Result` (was `Option`). `request_device(&desc)` lost its trace-path argument; `DeviceDescriptor.trace: wgpu::Trace`. |
| 26 | `as_hal` returns guards instead of taking callbacks. |
| 27 | `DeviceDescriptor.experimental_features`; `EXPERIMENTAL_*` features need `unsafe`. Buffer-mapping APIs no longer have lifetimes. `PollType::Wait` gained an optional timeout. |
| 28 | `enumerate_adapters` is **async**. Push constants are now "immediates" (`PipelineLayoutDescriptor.immediate_size`). `multiview_mask` on render pipeline and pass descriptors. Error scopes are guards (`device.push_error_scope(f)` → `.pop()`). `MipmapFilterMode` split out. Vulkan allocator switched to `gpu-allocator`. |
| 29 | **`InstanceDescriptor` lost `Default`/`from_env_or_default`**: use `new_with_display_handle[_from_env](Box<dyn ..>)` / `new_without_display_handle[_from_env]()`. **`Instance::new(desc)` by value again.** `Surface::get_current_texture()` returns the `CurrentSurfaceTexture` enum (Success / Suboptimal / Timeout / Occluded / Outdated / Lost / Validation). `PipelineLayoutDescriptor.bind_group_layouts: &[Option<&BindGroupLayout>]`. `DepthStencilState.depth_write_enabled` / `depth_compare` are `Option`. Write mappings are `WriteOnly<[u8]>`. `primitive_index` needs `enable primitive_index;`. |
| 30 | `queue.present(surface_texture)` replaces `SurfaceTexture::present()`. `VertexState.buffers: &[Option<VertexBufferLayout>]`. `SurfaceConfiguration.color_space` (HDR; `SurfaceColorSpace::Auto` keeps the old behaviour). `get_mapped_range*` return `Result`. `RequestAdapterOptions.apply_limit_buckets`. `dispatch` → `dispatch_workgroups`. `create_texture_from_hal(.., initial_state)`. Integer shader I/O needs an explicit `@interpolate(flat)`. `TextureUsages::TRANSIENT` → `TRANSIENT_ATTACHMENT`. |
| 31 (main, unreleased) | `DeviceDescriptor.default_queue: QueueDescriptor`. `TextureViewDescriptor.swizzle`. `BufferView` blocks unmap. **`VULKAN_EXTERNAL_MEMORY_{FD,DMA_BUF,WIN32}` feature flags removed** (extensions are turned on automatically; probe with `adapter.as_hal::<Vulkan>()`). |

---

## 5. egui_kittest snapshots headless on lavapipe (Q3)

**Yes. Verified.**

* **Features:** `egui_kittest = { features = ["wgpu", "snapshot"] }`.
  * `eframe` is only needed to test a whole `eframe::App`.
  * `video` (MP4 recording with `KITTEST_RECORD=1`) needs `ffmpeg` on the
    PATH. The README calls this feature `recording`, but the Cargo feature
    is named `video`.
* **Adapter choice:** `Harness::builder().wgpu()` (or just using
  `snapshot()` with the feature on) uses
  `egui_kittest::wgpu::default_wgpu_setup()`. That function **ranks
  `DeviceType::Cpu` adapters first**, so it picks lavapipe even when a real
  GPU is present, and it renders with `RendererOptions::PREDICTABLE` (no
  MSAA, no dithering, predictable texture filtering).
* **Testing our callbacks:** build the render state ourselves, register
  callback resources, then hand it to the harness:
  ```rust
  let rs = egui_kittest::wgpu::create_render_state(
      egui_kittest::wgpu::default_wgpu_setup(), egui_wgpu::RendererOptions::PREDICTABLE);
  rs.renderer.write().callback_resources.insert(ViewerResources::new(&rs.device, rs.target_format)); // Rgba8Unorm
  let mut h = egui_kittest::Harness::builder()
      .with_size(egui::vec2(160.0, 120.0))
      .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(rs))
      .build_ui(|ui| { /* … painter().add(Callback::new_paint_callback(rect, ViewerCallback)) … */ });
  h.run();
  h.snapshot("viewer_basic");        // or h.render()? -> image::RgbaImage for numeric asserts
  ```
* **Behaviour new in 0.37 to plan for:**
  * The harness **panics if an input widget has no accessible name**.
    Read-only text is exempt. Turn this off per harness with
    `.with_accessibility_check(false)`.
  * The harness panics on missing glyphs (see the font note in §2).
  * There is `HarnessBuilder::with_context_setup` for styles and fonts
    before the first pass.
* **Workflow:**
  * Snapshots go to `tests/snapshots/` by default.
  * Configure via `kittest.toml` at the workspace root: `output_path`,
    `threshold = 0.6`, `max_failed_pixels = 0`, and per-OS sections.
  * Run `UPDATE_SNAPSHOTS=1` to rewrite failing snapshots, or `=force` to
    rewrite all of them.
  * Add `**/tests/snapshots/**/*.diff.png` and `*.new.png` to `.gitignore`.
  * egui keeps its snapshots in Git LFS.
* **Build cost:** kittest turns on wgpu's `gles` (and no-op `metal`/`dx12`)
  in test builds, which adds EGL/glow compile time. Shipped binaries don't
  get it.

---

## 6. Wayland, fractional scaling and decorations in winit 0.30.13 (Q4)

* **Fractional scaling works.**
  * winit 0.30.13 binds `wp_fractional_scale_v1` + `wp_viewporter` when
    the compositor offers them (the buffer is physical size, the viewport
    destination is logical size). Otherwise it falls back to integer
    `wl_surface.buffer_scale`.
  * egui-winit turns `ScaleFactorChanged { scale_factor }` (f64, e.g. 1.25)
    into `native_pixels_per_point`. egui renders at physical pixels, so
    there is no blur.
  * Don't assume integer scale anywhere: in callback viewports, readback
    sizes, or golden-image sizes (use `PaintCallbackInfo::viewport_in_pixels()`).
  * KDE Plasma 6, Sway and Hyprland offer the protocol. GNOME/Mutter
    offers it when fractional scaling is turned on (the
    `scale-monitor-framebuffer` experimental setting on releases where it
    isn't on by default). Check the exact behaviour on the M1 GNOME
    machine.
* **Decorations: how winit 0.30 decides.**
  * With `ViewportBuilder::with_decorations(true)` (the default), winit
    requests **server-side** decorations through `zxdg_decoration_manager_v1`
    (`WindowDecorations::RequestServer`). Whatever the compositor answers in
    configure wins.
  * **KDE, Hyprland, Sway (and most wlroots/Smithay compositors): SSD.**
    Hyprland and Sway draw at most borders, which is normal there.
  * **GNOME/Mutter doesn't implement xdg-decoration**, so winit draws CSD.
    With winit's `wayland-csd-adwaita` feature that is an Adwaita-style
    frame (sctk-adwaita). Without it, it is sctk's bare `FallbackFrame`.
    `eframe` with `default-features = false` **drops** both
    `wayland-csd-adwaita` and `wayland-dlopen`, hence the direct `winit`
    entry in §2.
  * `with_decorations(false)` requests client mode and hides the frame,
    giving a borderless window. Then *we* would have to draw a title bar
    and use `ViewportCommand::StartDrag` / `BeginResize(..)`.
* **What that means for us:**
  * Keep `with_decorations(true)` and draw no title bar of our own.
  * Leave SSD vs CSD to the compositor, and keep `wayland-csd-adwaita` for
    GNOME.
  * Lay out using the inner size only; never assume outer size equals
    inner size.
  * Set `ViewportBuilder::with_app_id("…")` to match the `.desktop` file
    (eframe also derives its persistence path from it).
  * This covers the PLAN M1 "GNOME and Hyprland" check.
* **eframe 0.37 Wayland fixes that matter to us:**
  * It **keeps running when Wayland frame callbacks stop** (#8631). A
    hidden or occluded window used to stall the loop. Our design already
    lets the audio clock drive A/V, not repaints, so don't tie decode or
    playback to `request_repaint`.
  * PRIMARY selection.
  * The activation token is taken from the environment.
* **Linking libwayland:** with `wayland-dlopen`, libwayland-client is loaded
  at runtime. Without it, winit/wayland-sys link it at build time, which
  needs `libwayland-dev` + pkg-config.
* **winit 0.31** is at beta.3 and egui has only a WIP branch. Expect it in
  some later eframe. It rewrites the Wayland backend and its decoration
  handling, so look at this section again when eframe moves.

---

## 7. wgpu features for video planes on lavapipe / RADV / ANV (Q5)

How wgpu-hal 30 (Vulkan) decides each feature:

| Feature / format | wgpu-hal requirement | lavapipe (verified) | RADV / ANV (expected) |
|---|---|---|---|
| `R8Unorm`, `Rg8Unorm` (NV12 Y / UV planes) | Core WebGPU formats; no feature. Guaranteed filterable, renderable, storage | yes | yes |
| `TEXTURE_FORMAT_16BIT_NORM` (`R16Unorm`/`Rg16Unorm`, for P010) | R16, RG16, RGBA16 in both UNORM and SNORM, each with SAMPLED + STORAGE + TRANSFER in optimal tiling. Once on, they are guaranteed **filterable**, with storage usage | **yes** | yes: both drivers report `shaderStorageImageExtendedFormats`, which guarantees STORAGE for exactly these formats; sampling and transfer are universal on desktop |
| `TEXTURE_FORMAT_NV12` | `samplerYcbcrConversion` + `G8_B8R8_2PLANE_420_UNORM` with SAMPLED + TRANSFER_SRC/DST (MoltenVK excluded) | **yes** | yes (both drivers expose YCbCr conversion and two-plane 4:2:0) |
| `TEXTURE_FORMAT_P010` | same, with `G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16` | **yes** | yes |
| `EXTERNAL_TEXTURE` (WebGPU `GPUExternalTexture`) | DX12 and Metal only | no | **no on Vulkan**, so we convert YUV→RGB in WGSL ourselves |
| `VULKAN_EXTERNAL_MEMORY_FD` | `VK_KHR_external_memory_fd` | yes | yes |
| `VULKAN_EXTERNAL_MEMORY_DMA_BUF` | `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier` | **no** | yes (Mesa's Wayland WSI and compositors depend on these) |
| also seen on lavapipe | `SHADER_F16`, `FLOAT32_FILTERABLE`, `TIMESTAMP_QUERY`, `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` | yes | – |

What I verified on lavapipe (a full-screen YUV→RGB WGSL pass reading the
planes with `textureLoad`, BT.709 full range; input Y=0.6, U=0.4, V=0.7):

* **R8Unorm + Rg8Unorm** (NV12 layout, chroma at half resolution) gave
  `[234,134,106]`. The CPU reference is `[234,134,106]`.
* **R16Unorm + Rg16Unorm** with P010 packing (10 bits in the high bits,
  `v10 << 6`) gave `[233,134,105]` against the reference `[233,134,106]`,
  i.e. within 1 LSB. To be exact, scale the sample by `65535/65472` to undo
  the `<< 6`.
* **NV12 multi-planar texture:**
  * `queue.write_texture` per plane with `TextureAspect::Plane0` /
    `Plane1` works.
  * Plane views are made with `TextureViewDescriptor { format: R8Unorm / Rg8Unorm, aspect: Plane0 / Plane1 }`
    (P010 planes: `R16Unorm` / `Rg16Unorm`).
  * Result: `[234,134,106]`.
  * A *filterable* float binding of a plane view passed validation.

Limits of multi-planar textures that affect our design:

* `NV12`/`P010` textures allow only `COPY_SRC | COPY_DST | TEXTURE_BINDING`:
  no render-attachment or storage use, so **no GPU mip generation**.
* They also have a size rule: with mips, width and height must be a
  multiple of `2^mips`.
* The projection shader needs mipmaps plus `textureSampleGrad`
  (analytic LOD at the seam and poles). Use **separate per-plane textures**
  (R8 + RG8, or R16 + RG16) with `RENDER_ATTACHMENT | STORAGE_BINDING` so we
  can build mips on the GPU. Keep NV12/P010 multi-planar only as an import
  or copy *source*: since v30, `copy_texture_to_texture` can copy
  NV12 `Plane0` → `R8Unorm` and `Plane1` → `Rg8Unorm` (sizes in plane
  texels).

**DMA-buf zero-copy (VA-API) in wgpu 30** (compile-checked only):

```rust
// Needs VULKAN_EXTERNAL_MEMORY_DMA_BUF (removed as a feature flag in v31: probe the extension via as_hal).
let hal_tex = unsafe {
    device.as_hal::<wgpu::hal::api::Vulkan>().unwrap()
        .texture_from_dmabuf_fd(owned_fd, &wgpu::hal::TextureDescriptor { /* size, format: R8Unorm|Rg8Unorm|R16Unorm|Rg16Unorm,
                                 usage: wgpu::TextureUses::RESOURCE, memory_flags: empty, view_formats: vec![] .. */ },
                                drm_modifier, stride, offset)?
};
let tex = unsafe { device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(hal_tex, &desc, initial_state) };
```

* It imports **one memory plane only**: a single fd, modifier, stride and
  offset.
* For VA-API NV12/P010, export with `vaExportSurfaceHandle(..., VA_EXPORT_SURFACE_SEPARATE_LAYERS)`.
  That gives one layer per plane (`DRM_FORMAT_R8` / `GR88`, or
  `R16` / `GR1616`), and each layer maps onto one `R8Unorm` / `Rg8Unorm`
  (`R16Unorm` / `Rg16Unorm`) import. These are the same per-plane textures
  the software path uploads, so the shader doesn't change.
* Modifiers with auxiliary planes (AMD DCC, Intel CCS) can't go through
  this single-plane API, and neither can multi-planar dmabufs. Detect
  those and fall back to `av_hwframe_transfer_data` plus upload.
* `initial_state` (v30) tells wgpu what layout the image is already in.
  `TextureUses::UNINITIALIZED` → `UNDEFINED`, and that transition
  **discards contents** on compressed layouts.
* A proper `VK_QUEUE_FAMILY_FOREIGN_EXT` acquire barrier only arrives in v31
  (`hal::TextureBarrier::queue_family_ownership_transfer`).
* Treat zero-copy as an M1-hardware spike on RADV and ANV, not a
  dependency. The software path (decode → per-plane `write_texture`) is the
  baseline and is what runs on lavapipe.

Before relying on the "expected" column, run a one-off capability dump on
odin/huginn: the `list_adapters_and_features` test, which prints features
and `get_texture_format_features` per format. Record the output in the
hardware report.

---

## 8. Other crates: what to choose and why

* **glam 0.34.1 over nalgebra 0.35.**
  * Covers what we need without generics: `Quat`/`DQuat` (f32/f64),
    `slerp` (flips to the shortest arc, lerps when nearly parallel),
    `slerp_long` (keeps direction), `from_euler`/`to_euler(EulerRot::…)`,
    `from_axis_angle`, `DQuat::as_quat()`, and `Mat3`/`DMat3`.
  * `bytemuck` gives Pod uniforms; `serde` is available.
  * `libm` forces the same transcendental functions on every platform, if
    a proptest or golden reference ever has to match bit for bit.
  * It compiles fast and is permissively licensed.
  * nalgebra (`UnitQuaternion<T>`) is heavier and its glam interop stops at
    glam 0.33. Add it later only for real linear algebra (e.g. least-squares
    lens calibration in M3), and only in that crate.
  * Breaking change in 0.34: `look_at_*`/`perspective_*` were removed; use
    the `glam::camera` / `glam::dcamera` modules.
* **Our own `Rational` (already designed) over num-rational.** We need
  checked i128 intermediates, explicit rounding when rescaling PTS
  timebases, `const`, `Copy` and serde, and the existing design does that.
  num-rational 0.4.2 hasn't had a release since 2024-05, and `Ratio<i64>`
  arithmetic can overflow on intermediate products unless every call uses
  `Checked*`.
* **cpal 0.18.2.**
  * There are now native **PipeWire** (`pipewire` feature, pipewire-rs
    0.10, MIT) and **PulseAudio** (`pulseaudio` feature, a pure-Rust
    protocol client, MIT) hosts. At runtime the default host is the first
    available of PipeWire > PulseAudio > ALSA.
  * 0.18 changes we will hit:
    * streams no longer auto-start (call `play()`);
    * there is a unified `ErrorKind` with `Xrun` and `DeviceChanged`;
    * `StreamTrait::now()` gives the playback clock;
    * `StreamTrait::buffer_size()` exists.
  * Build requirements: ALSA is always compiled
    (**`libasound2-dev` + pkg-config**, which this research machine lacks,
    so I couldn't compile cpal here). `pipewire` needs
    **`libpipewire-0.3-dev` + libclang** (bindgen). Add these to
    `cargo xtask doctor`.
  * `BufferSize::Default` on ALSA can be huge. Use the native hosts, or a
    fixed buffer.
* **image 0.25.10** with `default-features = false, features = ["png"]`.
  This is the version eframe already requires, so we get no duplicate.
* **proptest 1.12.0** (MSRV 1.88), **criterion 0.8.2** (MSRV 1.86;
  `html_reports`), **clap 4.6.7**, **thiserror 2.0.21**,
  **anyhow 1.0.104**, **tracing 0.1.44** / **tracing-subscriber 0.3.23**,
  **serde 1.0.229**, **ron 0.12.2**. None of these conflict with each other.
* **egui_plot 0.38.0** is optional. It suits gyro/IMU telemetry and
  stabilization graphs (M4) and debug curves. Don't use it for video scopes
  (waveform, vectorscope, histogram); compute those with wgpu compute
  shaders as planned.

---

## 9. Watch list

* **egui_dock 0.22** release, if we want floating windows: PR #353 is open.
* **wgpu 31.**
  * Changes: `DeviceDescriptor.default_queue`, `TextureViewDescriptor.swizzle`,
    removal of the external-memory feature flags, the FOREIGN queue-family
    acquire, and a zero-init fix for multi-planar textures.
  * Adopt it only together with the egui release that moves egui-wgpu to 31.
* **winit 0.31 in eframe.** The Wayland backend and decorations change.
* **Mesa/LLVM versions on CI.** lavapipe golden images depend on them, so
  record `driver_info` alongside the goldens.
* **Fonts license decision** for `epaint_default_fonts` (§2).

## 10. How this was checked

* **Metadata:** crates.io API metadata, versions and dependency
  requirements for every crate above. `cargo info` for feature lists.
  Changelogs read from the published crate sources and upstream git
  (egui `main` @ bf5ac0d, wgpu `main` @ 63e5052, anhosh/egui_dock `main`
  plus PR #353, rerun-io/egui_crates).
* **Builds and tests**, in a scratch workspace using the §2 block (without
  cpal):
  * `cargo test` passed (4 headless wgpu tests, 2 kittest snapshot tests,
    1 NV12-filterable test).
  * kittest snapshots matched across three re-runs.
  * `cargo tree -e features` confirmed wgpu-hal is `vulkan` only in non-dev
    builds and that winit has `wayland-dlopen` + `wayland-csd-adwaita`.
  * The eframe app (CreationContext/Frame access, Vulkan-only
    `WgpuSetupCreateNew`, a custom adapter selector, a device-descriptor
    closure) and the DMA-buf import helper were compile-checked only.
* **Alternatives:**
  * egui 0.36.2 + egui_dock 0.21.1 + egui_tiles 0.17.1 + egui_plot 0.37.0
    + egui_kittest 0.36.2 on wgpu 30.0.1: `cargo check --tests` passed.
  * egui 0.37.0 + egui_dock @ PR #353: `cargo check` passed.
  * cpal 0.18.2 (`pipewire`, `pulseaudio`) + eframe 0.37: resolved, not
    compiled (no ALSA headers).
