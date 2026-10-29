//! Mesa's lavapipe for the tests that must see no GPU through Vulkan,
//! whatever the machine has: the loader pointed at lavapipe's ICD alone.

// Test helpers; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

/// Mesa's lavapipe ICD for this architecture. Tier 1 needs it; it ships in
/// mesa-vulkan-drivers (Fedora, Ubuntu) and vulkan-swrast (Arch).
pub fn lavapipe_icd() -> PathBuf {
    let dirs = [
        "/usr/share/vulkan/icd.d",
        "/etc/vulkan/icd.d",
        "/usr/local/share/vulkan/icd.d",
    ];
    let arch = std::env::consts::ARCH;
    let mut found: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with("lvp_icd") && (n == "lvp_icd.json" || n.contains(arch))
        })
        .collect();
    found.sort();
    found.into_iter().next().unwrap_or_else(|| {
        panic!(
            "no lavapipe Vulkan driver found in {dirs:?}; install mesa-vulkan-drivers \
             (Fedora, Ubuntu) or vulkan-swrast (Arch)"
        )
    })
}
