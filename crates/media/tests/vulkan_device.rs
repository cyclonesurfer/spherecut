//! `open_vulkan_device` on this machine: FFmpeg's Vulkan device runs on a
//! GPU whenever one is listed, and says which. Tier 1 needs FFmpeg's
//! Vulkan device, on lavapipe at least, as `hw_refusal.rs` does.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use spherecut_media::decode::{VulkanChoice, VulkanPick, open_vulkan_device};

fn open(render_node: Option<&Path>) -> VulkanChoice {
    open_vulkan_device(render_node).unwrap_or_else(|e| {
        panic!(
            "{e}; Tier 1 needs FFmpeg's Vulkan device, on Mesa's lavapipe at least \
             (mesa-vulkan-drivers on Fedora and Ubuntu, vulkan-swrast on Arch)"
        )
    })
}

#[test]
fn the_vulkan_device_is_a_gpu_whenever_one_is_listed() {
    let c = open(None);
    eprintln!("{c}");
    let gpus = c.devices.iter().filter(|d| !d.is_cpu()).count();
    assert_eq!(c.used.is_cpu(), gpus == 0, "{c}");
    assert_eq!(c.why == VulkanPick::NoGpu, gpus == 0, "{c}");
    // The listing marks the device the choice names, and only that one.
    let used: Vec<_> = c.devices.iter().filter(|d| d.used_by_ffmpeg).collect();
    assert_eq!(used.len(), 1, "{:?}", c.devices);
    assert_eq!(
        (&used[0].name, &used[0].device_id),
        (&c.used.name, &c.used.device_id)
    );
    if gpus == 0 {
        assert!(
            c.to_string()
                .ends_with("every Vulkan device listed runs on the CPU"),
            "{c}"
        );
    }
}

#[test]
fn without_its_render_node_the_first_gpu_is_used() {
    let node = Path::new("/nonexistent/renderD128");
    let c = open(Some(node));
    match &c.why {
        VulkanPick::FirstGpu { unmatched } => {
            assert_eq!(unmatched.as_deref(), Some(node));
            assert!(!c.used.is_cpu(), "{c}");
        }
        VulkanPick::NoGpu => assert!(c.devices.iter().all(|d| d.is_cpu()), "{c}"),
        VulkanPick::Node(_) => panic!("chose by a node that is not there: {c}"),
    }
}
