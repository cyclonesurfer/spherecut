//! What hw-test reads about the machine, and only reads (M1 plan section
//! 12.4): the power settings that move decode speed, every DRM render
//! node, whether lavapipe's ICD is installed, and the Rust toolchain's
//! version. Each reads under a root it is given: `/` on the machine, a
//! fixture tree in the tests.

use std::path::{Path, PathBuf};

/// The oldest rustc that builds Spherecut: the workspace's
/// `rust-version`.
pub(crate) const MIN_RUST: (u32, u32) = (1, 95);

/// `rustc -V`'s version, "rustc 1.99.0 (…)" giving (1, 99).
pub(crate) fn rustc_version(text: &str) -> Option<(u32, u32)> {
    let v = text.split_whitespace().nth(1)?;
    let mut parts = v.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// A sysfs value, trimmed; `None` when it can't be read or is empty.
fn read(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// The power settings that move decode speed, as (setting, value) lines
/// for the report: power-profiles-daemon's profile (`profiles_daemon`,
/// `powerprofilesctl get`'s output when it ran), ACPI's platform profile
/// and its choices, the first CPU's frequency driver, governor and
/// energy preference, and each amdgpu card's forced performance level
/// and active power profile. huginn's X5 8K30 decoded at 1.17× to 2.41×
/// real time from one run to the next; these say whether the machine
/// was set the same way.
pub(crate) fn power_profile(root: &Path, profiles_daemon: Option<&str>) -> Vec<(String, String)> {
    let missing = |what: &str| format!("not available ({what})");
    let mut out = vec![(
        "power-profiles-daemon".to_string(),
        profiles_daemon
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| missing("no `powerprofilesctl`"), str::to_string),
    )];
    let acpi = root.join("sys/firmware/acpi");
    let platform = read(&acpi.join("platform_profile")).map_or_else(
        || missing("no /sys/firmware/acpi/platform_profile"),
        |p| match read(&acpi.join("platform_profile_choices")) {
            Some(choices) => format!("{p} (of {choices})"),
            None => p,
        },
    );
    out.push(("ACPI platform profile".into(), platform));
    let cpufreq = root.join("sys/devices/system/cpu/cpu0/cpufreq");
    for (file, what) in [
        ("scaling_driver", "CPU frequency driver"),
        ("scaling_governor", "CPU governor"),
        ("energy_performance_preference", "CPU energy preference"),
    ] {
        let v = read(&cpufreq.join(file))
            .unwrap_or_else(|| missing(&format!("no cpu0/cpufreq/{file}")));
        out.push((what.into(), v));
    }
    for card in amdgpu_cards(root) {
        let device = root.join("sys/class/drm").join(&card).join("device");
        let level = read(&device.join("power_dpm_force_performance_level"))
            .unwrap_or_else(|| missing("unreadable"));
        out.push((format!("{card} (amdgpu) performance level"), level));
        let mode = std::fs::read_to_string(device.join("pp_power_profile_mode"))
            .ok()
            .and_then(|t| active_power_profile(&t))
            .unwrap_or_else(|| missing("unreadable"));
        out.push((format!("{card} (amdgpu) power profile"), mode));
    }
    out
}

/// The `card<n>` entries of `/sys/class/drm` whose driver is amdgpu, by
/// name.
fn amdgpu_cards(root: &Path) -> Vec<String> {
    let mut cards: Vec<String> = entries(&root.join("sys/class/drm"))
        .into_iter()
        .filter(|n| {
            n.strip_prefix("card")
                .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty())
        })
        .filter(|n| {
            let dev = root.join("sys/class/drm").join(n).join("device");
            uevent(&dev, "DRIVER").as_deref() == Some("amdgpu")
        })
        .collect();
    cards.sort_by_key(|n| node_number(n));
    cards
}

/// amdgpu's `pp_power_profile_mode`: the mode marked with `*`. Newer
/// kernels list `  1 3D_FULL_SCREEN*:` and older ones
/// `  1 3D_FULL_SCREEN *:` before each mode's numbers.
fn active_power_profile(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (head, _) = l.split_once(':')?;
        if !head.contains('*') {
            return None;
        }
        let mut words = head.split_whitespace();
        words.next()?.parse::<u32>().ok()?;
        let name: String = words.collect::<Vec<_>>().join(" ");
        let name = name.replace('*', "");
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_string())
    })
}

/// A value of a sysfs device's `uevent`, such as `DRIVER` or
/// `PCI_SLOT_NAME`.
fn uevent(device: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(device.join("uevent")).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k == key && !v.trim().is_empty()).then(|| v.trim().to_string())
    })
}

/// The names in a directory; none when it can't be read.
fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect()
}

/// `renderD129` or `card1` giving 129 or 1, for sorting.
fn node_number(name: &str) -> u32 {
    let digits: String = name.chars().filter(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(u32::MAX)
}

/// Whether this user can open a render node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Access {
    Yes,
    /// With why: no node in /dev/dri, or the error opening it.
    No(String),
}

/// A DRM render node, from sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderNode {
    pub name: String,
    pub pci_slot: Option<String>,
    pub vendor: Option<String>,
    pub device: Option<String>,
    pub driver: Option<String>,
    pub access: Access,
}

impl RenderNode {
    /// "AMD (0x1002)", or the bare id for another vendor.
    pub(crate) fn vendor_name(&self) -> String {
        let Some(id) = &self.vendor else {
            return "?".into();
        };
        let name = match id.as_str() {
            "0x1002" => "AMD",
            "0x8086" => "Intel",
            "0x10de" => "NVIDIA",
            "0x1af4" => "virtio",
            _ => return id.clone(),
        };
        format!("{name} ({id})")
    }
}

/// Every `renderD<n>` in `/sys/class/drm` under `root`, by number, with
/// its PCI slot, ids and driver, and whether its node in `dev_dri` opens
/// for reading and writing, as a decoder opens it. Opening a render node
/// and closing it again changes nothing.
pub(crate) fn render_nodes(root: &Path, dev_dri: &Path) -> Vec<RenderNode> {
    let class = root.join("sys/class/drm");
    let mut names: Vec<String> = entries(&class)
        .into_iter()
        .filter(|n| n.starts_with("renderD"))
        .collect();
    names.sort_by_key(|n| node_number(n));
    names
        .into_iter()
        .map(|name| {
            let dev = class.join(&name).join("device");
            let node = dev_dri.join(&name);
            let access = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&node)
            {
                Ok(_) => Access::Yes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Access::No(format!("no {}", node.display()))
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    Access::No("permission denied: is this user in the `render` group?".into())
                }
                Err(e) => Access::No(e.to_string()),
            };
            RenderNode {
                pci_slot: uevent(&dev, "PCI_SLOT_NAME"),
                vendor: read(&dev.join("vendor")),
                device: read(&dev.join("device")),
                driver: uevent(&dev, "DRIVER"),
                access,
                name,
            }
        })
        .collect()
}

/// Where the Vulkan loader looks for ICD manifests, under `root`.
const ICD_DIRS: [&str; 3] = [
    "usr/share/vulkan/icd.d",
    "etc/vulkan/icd.d",
    "usr/local/share/vulkan/icd.d",
];

/// lavapipe's ICD manifest (`lvp_icd*.json`), which the Tier 1 render
/// and playback tests render on, if one is installed.
pub(crate) fn lavapipe_icd(root: &Path) -> Option<PathBuf> {
    ICD_DIRS.iter().find_map(|d| {
        let dir = root.join(d);
        let mut names = entries(&dir);
        names.sort();
        names
            .into_iter()
            .find(|n| n.starts_with("lvp_icd") && n.ends_with(".json"))
            .map(|n| dir.join(n))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture tree in the temp directory, removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str, files: &[(&str, &str)]) -> Tree {
            let dir = std::env::temp_dir().join(format!(
                "spherecut-xtask-machine-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for (rel, text) in files {
                let p = dir.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
            Tree(dir)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_minimum_rust_is_the_workspaces() {
        let manifest = include_str!("../../Cargo.toml");
        let line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("rust-version"))
            .unwrap();
        let v = line.split('"').nth(1).unwrap();
        assert_eq!(rustc_version(&format!("rustc {v}")), Some(MIN_RUST));
    }

    #[test]
    fn reads_rustcs_version() {
        assert_eq!(
            rustc_version("rustc 1.99.0 (2d8144b78 2026-07-07)\n"),
            Some((1, 99))
        );
        assert_eq!(
            rustc_version("rustc 1.96.0-nightly (abc 2026-05-01)"),
            Some((1, 96))
        );
        assert_eq!(rustc_version("rustc 1.95"), Some((1, 95)));
        assert!(rustc_version("rustc 1.94.1 (x)").unwrap() < MIN_RUST);
        assert_eq!(rustc_version("could not run `rustc`"), None);
        assert_eq!(rustc_version(""), None);
    }

    /// As on a desktop with an amdgpu card at 0000:03:00.0 (card1,
    /// renderD129), an Intel iGPU (card0, renderD128) and a connector.
    fn desktop() -> Tree {
        let amd = "DRIVER=amdgpu\nPCI_CLASS=30000\nPCI_SLOT_NAME=0000:03:00.0\n";
        let intel = "DRIVER=i915\nPCI_SLOT_NAME=0000:00:02.0\n";
        Tree::new(
            "desktop",
            &[
                ("sys/firmware/acpi/platform_profile", "balanced\n"),
                (
                    "sys/firmware/acpi/platform_profile_choices",
                    "low-power balanced performance\n",
                ),
                (
                    "sys/devices/system/cpu/cpu0/cpufreq/scaling_driver",
                    "amd-pstate-epp\n",
                ),
                (
                    "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
                    "powersave\n",
                ),
                ("sys/class/drm/card1/device/uevent", amd),
                (
                    "sys/class/drm/card1/device/power_dpm_force_performance_level",
                    "auto\n",
                ),
                (
                    "sys/class/drm/card1/device/pp_power_profile_mode",
                    "PROFILE_INDEX(NAME) CLOCK_TYPE(NAME) FPS MinActiveFreqType\n \
                     0 BOOTUP_DEFAULT :\n                     0(       GFXCLK)       0       5\n \
                     1 3D_FULL_SCREEN*:\n                     0(       GFXCLK)       1       5\n \
                     2   POWER_SAVING :\n",
                ),
                ("sys/class/drm/card1-DP-1/status", "connected\n"),
                ("sys/class/drm/card0/device/uevent", intel),
                ("sys/class/drm/renderD129/device/uevent", amd),
                ("sys/class/drm/renderD129/device/vendor", "0x1002\n"),
                ("sys/class/drm/renderD129/device/device", "0x7480\n"),
                ("sys/class/drm/renderD128/device/uevent", intel),
                ("sys/class/drm/renderD128/device/vendor", "0x8086\n"),
                ("sys/class/drm/renderD128/device/device", "0x9a49\n"),
                ("dev/dri/renderD129", ""),
            ],
        )
    }

    #[test]
    fn reads_the_power_settings_that_move_decode_speed() {
        let t = desktop();
        let p = power_profile(&t.0, Some("performance\n"));
        let get = |k: &str| {
            p.iter()
                .find(|(what, _)| what == k)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("no {k} in {p:?}"))
        };
        assert_eq!(get("power-profiles-daemon"), "performance");
        assert_eq!(
            get("ACPI platform profile"),
            "balanced (of low-power balanced performance)"
        );
        assert_eq!(get("CPU frequency driver"), "amd-pstate-epp");
        assert_eq!(get("CPU governor"), "powersave");
        assert_eq!(
            get("CPU energy preference"),
            "not available (no cpu0/cpufreq/energy_performance_preference)"
        );
        assert_eq!(get("card1 (amdgpu) performance level"), "auto");
        assert_eq!(get("card1 (amdgpu) power profile"), "3D_FULL_SCREEN");
        // The Intel card and the connector aren't amdgpu cards.
        assert_eq!(p.iter().filter(|(k, _)| k.contains("amdgpu")).count(), 2);
    }

    #[test]
    fn a_machine_without_the_settings_says_so() {
        let t = Tree::new("bare", &[("sys/.keep", "")]);
        let p = power_profile(&t.0, None);
        assert_eq!(p.len(), 5, "{p:?}");
        assert!(
            p.iter().all(|(_, v)| v.starts_with("not available")),
            "{p:?}"
        );
    }

    #[test]
    fn finds_the_active_power_profile_in_either_layout() {
        let old = "NUM        MODE_NAME     SCLK_UP_HYST   SCLK_DOWN_HYST\n  \
                   0   BOOTUP_DEFAULT:        -                -\n  \
                   1 3D_FULL_SCREEN *:        0              100\n  \
                   2     POWER_SAVING:       10                0\n";
        assert_eq!(active_power_profile(old).as_deref(), Some("3D_FULL_SCREEN"));
        let new = " 0 BOOTUP_DEFAULT*:\n      0(GFXCLK) 0 5\n 1 COMPUTE :\n";
        assert_eq!(active_power_profile(new).as_deref(), Some("BOOTUP_DEFAULT"));
        assert_eq!(active_power_profile("NUM MODE_NAME\n 0 X :\n"), None);
    }

    #[test]
    fn lists_every_render_node_with_its_device() {
        let t = desktop();
        let nodes = render_nodes(&t.0, &t.0.join("dev/dri"));
        assert_eq!(nodes.len(), 2, "{nodes:?}");
        assert_eq!(nodes[0].name, "renderD128");
        assert_eq!(nodes[0].pci_slot.as_deref(), Some("0000:00:02.0"));
        assert_eq!(nodes[0].vendor_name(), "Intel (0x8086)");
        assert_eq!(nodes[0].device.as_deref(), Some("0x9a49"));
        assert_eq!(nodes[0].driver.as_deref(), Some("i915"));
        assert!(
            matches!(&nodes[0].access, Access::No(why) if why.contains("renderD128")),
            "{:?}",
            nodes[0].access
        );
        assert_eq!(
            nodes[1],
            RenderNode {
                name: "renderD129".into(),
                pci_slot: Some("0000:03:00.0".into()),
                vendor: Some("0x1002".into()),
                device: Some("0x7480".into()),
                driver: Some("amdgpu".into()),
                access: Access::Yes,
            }
        );
        assert_eq!(nodes[1].vendor_name(), "AMD (0x1002)");
        let none = Tree::new("no-drm", &[("sys/.keep", "")]);
        assert!(render_nodes(&none.0, &none.0.join("dev/dri")).is_empty());
    }

    #[test]
    fn finds_lavapipes_icd_where_the_loader_looks() {
        let t = Tree::new(
            "icd",
            &[
                ("usr/share/vulkan/icd.d/radeon_icd.x86_64.json", "{}"),
                ("etc/vulkan/icd.d/lvp_icd.x86_64.json", "{}"),
            ],
        );
        assert_eq!(
            lavapipe_icd(&t.0),
            Some(t.0.join("etc/vulkan/icd.d/lvp_icd.x86_64.json"))
        );
        let none = Tree::new(
            "no-icd",
            &[("usr/share/vulkan/icd.d/radeon_icd.json", "{}")],
        );
        assert_eq!(lavapipe_icd(&none.0), None);
    }
}
