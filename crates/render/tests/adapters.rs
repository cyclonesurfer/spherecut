//! What each Vulkan adapter can do for the renderer (M1 plan section
//! 12.2): `list_adapters_and_features` records every adapter for the
//! hw-test report and fails if one can't render into and filter a
//! canonical format; its Tier 1 smoke run checks lavapipe the same way.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::gpu;
use spherecut_render::{Gpu, LinearTarget, PlaneFormat};
use spherecut_testkit::{bench, tier2};
use wgpu::{TextureFormat, TextureFormatFeatureFlags, TextureUsages};

/// The formats the plan lists, as each adapter supports them.
const LISTED: [TextureFormat; 8] = [
    TextureFormat::R8Unorm,
    TextureFormat::Rg8Unorm,
    TextureFormat::R16Uint,
    TextureFormat::R16Float,
    TextureFormat::Rg16Float,
    TextureFormat::Rgba16Float,
    TextureFormat::NV12,
    TextureFormat::P010,
];

/// What the renderer needs of `format`, or `None` if it doesn't use it:
/// the plane textures and the linear target are rendered into (the mips
/// and the projection) and filtered; the deep planes' integer scratch is
/// written by copies and read with `textureLoad`.
fn needed(format: TextureFormat) -> Option<(TextureUsages, TextureFormatFeatureFlags)> {
    let canonical =
        PlaneFormat::ALL.iter().any(|p| p.texture() == format) || format == LinearTarget::FORMAT;
    let scratch = PlaneFormat::ALL.iter().any(|p| p.scratch() == Some(format));
    if canonical {
        Some((
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            TextureFormatFeatureFlags::FILTERABLE,
        ))
    } else if scratch {
        Some((
            TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            TextureFormatFeatureFlags::empty(),
        ))
    } else {
        None
    }
}

/// The formats the renderer needs: the listed ones it uses, and Rg16Uint,
/// the two-component scratch.
fn checked() -> Vec<TextureFormat> {
    let mut v: Vec<TextureFormat> = LISTED
        .into_iter()
        .filter(|f| needed(*f).is_some())
        .collect();
    v.push(TextureFormat::Rg16Uint);
    v
}

/// An adapter as the report shows it, in a few notes, and what it lacks
/// of what the renderer needs.
fn survey(adapter: &wgpu::Adapter, gpu: Option<&Gpu>) -> (Vec<String>, Vec<String>) {
    let info = adapter.get_info();
    let name = format!("{} ({:?})", info.name, info.device_type);
    let period = gpu.map_or("no device opened".into(), |g| {
        format!(
            "{} ns per tick, timestamps {} and inside encoders {}",
            g.caps.timestamp_period_ns, g.caps.timestamps, g.caps.timestamps_in_encoders
        )
    });
    let mut notes = vec![format!(
        "adapter {name}: driver {} {}, vendor {:#06x}, device {:#06x}, PCI bus id {}, backend {:?}; \
         timestamp period: {period}",
        info.driver,
        info.driver_info,
        info.vendor,
        info.device,
        if info.device_pci_bus_id.is_empty() {
            "(none)"
        } else {
            &info.device_pci_bus_id
        },
        info.backend,
    )];
    let formats: Vec<String> = LISTED
        .iter()
        .map(|&f| {
            let ff = adapter.get_texture_format_features(f);
            format!("{f:?}: {:?}, {:?}", ff.allowed_usages, ff.flags)
        })
        .collect();
    notes.push(format!("adapter {name} formats: {}", formats.join("; ")));
    notes.push(format!("adapter {name} features: {:?}", adapter.features()));
    let down = adapter.get_downlevel_capabilities();
    notes.push(format!(
        "adapter {name} downlevel: {:?}, shader model {:?}",
        down.flags, down.shader_model
    ));
    notes.push(format!("adapter {name} limits: {:?}", adapter.limits()));
    let mut lacks = Vec::new();
    for f in checked() {
        let Some((usages, flags)) = needed(f) else {
            continue;
        };
        let ff = adapter.get_texture_format_features(f);
        if !ff.allowed_usages.contains(usages) || !ff.flags.contains(flags) {
            lacks.push(format!(
                "{name} can't use {f:?} as the renderer does: it needs {usages:?} and {flags:?}, \
                 and has {:?} and {:?}",
                ff.allowed_usages, ff.flags
            ));
        }
    }
    (notes, lacks)
}

#[test]
fn lavapipe_has_what_the_renderer_needs() {
    // The Tier 2 test's survey, on Tier 1's adapter.
    let gpu = gpu();
    let (notes, lacks) = survey(&gpu.adapter, Some(gpu));
    assert!(lacks.is_empty(), "{lacks:#?}");
    assert!(notes.len() == 5 && notes[1].contains("NV12") && notes[0].contains("llvmpipe"));
}

#[test]
#[ignore = "tier2: lists the machine's Vulkan adapters"]
fn list_adapters_and_features() {
    tier2::require_hw_run();
    let (gpus, _) = common::hardware_gpus();
    let instance = wgpu::Instance::new(Gpu::instance_descriptor());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    let mut lacks = Vec::new();
    let mut seen = Vec::new();
    for a in &adapters {
        let info = a.get_info();
        // The device opened on this adapter, for its timestamp period;
        // lavapipe's is the Tier 1 one.
        let cpu;
        let device = if info.device_type == wgpu::DeviceType::Cpu {
            cpu = Gpu::headless(&spherecut_render::AdapterChoice::Cpu).ok();
            cpu.as_ref()
        } else {
            gpus.iter().find(|g| {
                g.caps.info.name == info.name
                    && g.caps.info.device_pci_bus_id == info.device_pci_bus_id
            })
        };
        let (notes, l) = survey(a, device);
        for n in notes {
            bench::note(n);
        }
        lacks.extend(l);
        seen.push(format!("{} ({:?})", info.name, info.device_type));
    }
    assert!(lacks.is_empty(), "{lacks:#?}");
    if gpus.is_empty() {
        tier2::skip(format!(
            "no GPU: Vulkan lists only {}, which has what the renderer needs",
            seen.join(", ")
        ));
    }
}
