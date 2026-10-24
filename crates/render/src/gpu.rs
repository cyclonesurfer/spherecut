//! The wgpu instance, adapter, device and queue (M1 plan, section 4.1).
//!
//! Every device Spherecut renders on is made here, from one instance
//! descriptor (Vulkan only), one adapter rule and one device descriptor,
//! so that `spherecut frame`, the tests and the GUI (through
//! [`Gpu::from_parts`], with eframe's device) agree on features and
//! limits. Slice 5b creates the Vulkan device itself, through wgpu-hal's
//! `Adapter::open_with_callback`, to add the video-decode extensions and
//! queues FFmpeg's Vulkan Video needs and the DMA-BUF import extensions,
//! then wraps it with `wgpu::Adapter::create_device_from_hal`; that
//! replaces [`open_device`] and nothing else (DECISIONS D31).

use std::fmt;

use crate::error::RenderError;

/// Which adapter to render on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterChoice {
    /// The best there is: a discrete GPU, then an integrated one, then a
    /// virtual one, then a CPU (lavapipe), so that a box without a GPU
    /// still renders.
    Auto,
    /// A CPU adapter (lavapipe): what the Tier 1 tests and goldens use.
    Cpu,
    /// The adapter whose name contains this (case-insensitively), or
    /// whose PCI bus id is this.
    Named(String),
}

impl AdapterChoice {
    /// The environment variable [`AdapterChoice::from_env`] reads: `auto`,
    /// `cpu`, or a name or bus id. egui-wgpu ignores wgpu's own
    /// `WGPU_ADAPTER_NAME`, so Spherecut has its own.
    pub const ENV: &'static str = "SPHERECUT_ADAPTER";

    /// `auto`, `cpu` or a name.
    pub fn parse(s: &str) -> AdapterChoice {
        match s.trim() {
            t if t.eq_ignore_ascii_case("auto") || t.is_empty() => AdapterChoice::Auto,
            t if t.eq_ignore_ascii_case("cpu") => AdapterChoice::Cpu,
            t => AdapterChoice::Named(t.to_string()),
        }
    }

    /// [`AdapterChoice::ENV`]'s choice, `None` when it isn't set. Callers
    /// that honour it (the CLI and the GUI) read it once; tests pass their
    /// choice instead, so nothing changes the environment in-process.
    pub fn from_env() -> Option<AdapterChoice> {
        std::env::var(Self::ENV).ok().map(|v| Self::parse(&v))
    }
}

impl fmt::Display for AdapterChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterChoice::Auto => f.write_str("auto"),
            AdapterChoice::Cpu => f.write_str("cpu"),
            AdapterChoice::Named(n) => f.write_str(n),
        }
    }
}

/// What the device can do, as far as the renderer cares.
#[derive(Debug, Clone)]
pub struct GpuCaps {
    pub info: wgpu::AdapterInfo,
    /// A CPU adapter (lavapipe): correct but slow. The GUI warns.
    pub is_cpu: bool,
    /// `TIMESTAMP_QUERY`: timestamps at the start and end of passes.
    pub timestamps: bool,
    /// `TIMESTAMP_QUERY_INSIDE_ENCODERS`: timestamps between passes, which
    /// [`crate::GpuTimers`] uses.
    pub timestamps_in_encoders: bool,
    /// `DownlevelFlags::ANISOTROPIC_FILTERING`. Without it wgpu makes
    /// every sampler isotropic, so [`crate::Filter::Anisotropic`] is the
    /// same as [`crate::Filter::Isotropic`] and the GUI greys it out.
    pub anisotropy: bool,
    /// The largest 2D texture the device was asked for (the adapter's,
    /// capped at 16384).
    pub max_texture_2d: u32,
    /// Nanoseconds per timestamp tick.
    pub timestamp_period_ns: f32,
}

/// One device and its queue, with the instance and adapter it came from.
/// Cloning shares them (wgpu's handles are reference counted).
#[derive(Debug, Clone)]
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub caps: GpuCaps,
}

/// The cap on texture size asked of any adapter. RADV and lavapipe report
/// 16384; wgpu's default, 8192, is too small for 11K sources.
pub const MAX_TEXTURE_2D: u32 = 16384;

impl Gpu {
    /// A device for rendering without a window: `spherecut frame`, the
    /// tests and the benchmarks.
    pub fn headless(choice: &AdapterChoice) -> Result<Gpu, RenderError> {
        let instance = wgpu::Instance::new(Self::instance_descriptor());
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let adapter = Self::select_adapter(&adapters, choice)?;
        let (device, queue) = open_device(&adapter)?;
        tracing::info!(adapter = %describe(&adapter.get_info()), %choice, "rendering headless");
        Ok(Self::from_parts(instance, adapter, device, queue))
    }

    /// Wraps a device made elsewhere: eframe's, which it makes from
    /// [`Gpu::instance_descriptor`], [`Gpu::select_adapter`] and
    /// [`Gpu::device_descriptor`] (slice 7).
    pub fn from_parts(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Gpu {
        let info = adapter.get_info();
        let features = device.features();
        let caps = GpuCaps {
            is_cpu: info.device_type == wgpu::DeviceType::Cpu,
            info,
            timestamps: features.contains(wgpu::Features::TIMESTAMP_QUERY),
            timestamps_in_encoders: features.contains(
                wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
            ),
            anisotropy: adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING),
            max_texture_2d: device.limits().max_texture_dimension_2d,
            timestamp_period_ns: queue.get_timestamp_period(),
        };
        Gpu {
            instance,
            adapter,
            device,
            queue,
            caps,
        }
    }

    /// The Vulkan backend only, with wgpu's default flags (validation in
    /// debug builds, when the layers are installed).
    pub fn instance_descriptor() -> wgpu::InstanceDescriptor {
        wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        }
    }

    /// The adapter `choice` names among `adapters`.
    pub fn select_adapter(
        adapters: &[wgpu::Adapter],
        choice: &AdapterChoice,
    ) -> Result<wgpu::Adapter, RenderError> {
        let infos: Vec<wgpu::AdapterInfo> = adapters.iter().map(wgpu::Adapter::get_info).collect();
        let index = pick(&infos, choice);
        match index.and_then(|i| adapters.get(i)) {
            Some(a) => Ok(a.clone()),
            None if adapters.is_empty() => Err(RenderError::NoAdapter {
                tried: vec!["Vulkan".into()],
            }),
            None => Err(RenderError::AdapterNotFound {
                wanted: choice.to_string(),
                available: infos.iter().map(describe).collect(),
            }),
        }
    }

    /// What every device is asked for:
    /// - the adapter's timestamp features, when it has them;
    /// - wgpu's default limits, with 2D textures raised to the adapter's
    ///   own limit, capped at [`MAX_TEXTURE_2D`];
    /// - no texture format features: every format the renderer uses is
    ///   guaranteed without them (DECISIONS D31);
    /// - memory hints for performance.
    pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
        let wanted =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        let limits = adapter.limits();
        wgpu::DeviceDescriptor {
            label: Some("spherecut"),
            required_features: adapter.features() & wanted,
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: limits.max_texture_dimension_2d.min(MAX_TEXTURE_2D),
                ..wgpu::Limits::default()
            },
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }
    }

    /// `TooLarge` when a `w` × `h` texture doesn't fit this device.
    pub fn check_size(&self, (w, h): (u32, u32)) -> Result<(), RenderError> {
        let max = self.caps.max_texture_2d;
        if w > max || h > max {
            return Err(RenderError::TooLarge { w, h, max });
        }
        Ok(())
    }
}

/// The device and queue for `adapter`: where slice 5b makes its own
/// Vulkan device instead (module docs).
fn open_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), RenderError> {
    pollster::block_on(adapter.request_device(&Gpu::device_descriptor(adapter))).map_err(|e| {
        RenderError::Device(format!("{} refused a device: {e}", adapter.get_info().name))
    })
}

/// An adapter as the errors list it: its name, kind and driver.
pub fn describe(info: &wgpu::AdapterInfo) -> String {
    let kind = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => "discrete",
        wgpu::DeviceType::IntegratedGpu => "integrated",
        wgpu::DeviceType::VirtualGpu => "virtual",
        wgpu::DeviceType::Cpu => "CPU",
        wgpu::DeviceType::Other => "other",
    };
    let bus = if info.device_pci_bus_id.is_empty() {
        String::new()
    } else {
        format!(", PCI {}", info.device_pci_bus_id)
    };
    format!(
        "{} ({kind}, {} {}{bus})",
        info.name, info.driver, info.driver_info
    )
}

/// The index of the adapter `choice` names, by [`AdapterChoice`]'s rules.
fn pick(infos: &[wgpu::AdapterInfo], choice: &AdapterChoice) -> Option<usize> {
    let rank = |t: wgpu::DeviceType| match t {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Cpu => 3,
        wgpu::DeviceType::Other => 4,
    };
    match choice {
        // The first of the best kind, in the order Vulkan lists them.
        AdapterChoice::Auto => (0..infos.len()).min_by_key(|&i| (rank(infos[i].device_type), i)),
        AdapterChoice::Cpu => infos
            .iter()
            .position(|i| i.device_type == wgpu::DeviceType::Cpu),
        AdapterChoice::Named(want) => {
            let want_lower = want.to_lowercase();
            infos
                .iter()
                .position(|i| i.device_pci_bus_id == *want)
                .or_else(|| {
                    infos
                        .iter()
                        .position(|i| i.name.to_lowercase().contains(&want_lower))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, t: wgpu::DeviceType, bus: &str) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: name.into(),
            device_type: t,
            device_pci_bus_id: bus.into(),
            ..wgpu::AdapterInfo::new(wgpu::DeviceType::Other, wgpu::Backend::Vulkan)
        }
    }

    #[test]
    fn adapters_are_picked_by_kind_name_and_bus() {
        use wgpu::DeviceType::*;
        let infos = [
            info("llvmpipe (LLVM 20.1.2, 256 bits)", Cpu, ""),
            info(
                "AMD Radeon 8060S Graphics (RADV GFX1151)",
                IntegratedGpu,
                "0000:c5:00.0",
            ),
            info(
                "AMD Radeon RX 7600 (RADV NAVI33)",
                DiscreteGpu,
                "0000:03:00.0",
            ),
        ];
        assert_eq!(pick(&infos, &AdapterChoice::Auto), Some(2));
        assert_eq!(pick(&infos[..2], &AdapterChoice::Auto), Some(1));
        assert_eq!(pick(&infos[..1], &AdapterChoice::Auto), Some(0));
        assert_eq!(pick(&infos, &AdapterChoice::Cpu), Some(0));
        assert_eq!(pick(&infos[1..], &AdapterChoice::Cpu), None);
        assert_eq!(pick(&infos, &AdapterChoice::parse("8060s")), Some(1));
        assert_eq!(pick(&infos, &AdapterChoice::parse("0000:03:00.0")), Some(2));
        assert_eq!(pick(&infos, &AdapterChoice::parse("intel")), None);
        assert_eq!(pick(&[], &AdapterChoice::Auto), None);
    }

    #[test]
    fn choices_parse_and_print() {
        assert_eq!(AdapterChoice::parse(" Auto "), AdapterChoice::Auto);
        assert_eq!(AdapterChoice::parse(""), AdapterChoice::Auto);
        assert_eq!(AdapterChoice::parse("CPU"), AdapterChoice::Cpu);
        assert_eq!(
            AdapterChoice::parse("RX 7600"),
            AdapterChoice::Named("RX 7600".into())
        );
        assert_eq!(
            AdapterChoice::Named("RX 7600".into()).to_string(),
            "RX 7600"
        );
    }

    #[test]
    fn errors_list_what_there_is() {
        let e = Gpu::select_adapter(&[], &AdapterChoice::Auto).unwrap_err();
        assert_eq!(e.code(), "no_gpu");
        let d = describe(&info("llvmpipe", wgpu::DeviceType::Cpu, ""));
        assert!(d.starts_with("llvmpipe (CPU"), "{d}");
    }
}
