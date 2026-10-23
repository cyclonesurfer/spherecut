//! `spherecut doctor`: which codecs and hardware paths this machine has,
//! and the exact command that fixes anything missing.
//!
//! The GUI runs the same check at startup so a missing HEVC decoder is
//! reported up front, not as a decode error halfway through a project.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::{ff, ffi, init};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Distro {
    Fedora,
    Arch,
    Other,
}

impl Distro {
    pub fn detect() -> Self {
        let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        let ids: String = os
            .lines()
            .filter(|l| l.starts_with("ID=") || l.starts_with("ID_LIKE="))
            .collect::<Vec<_>>()
            .join(" ");
        if ids.contains("fedora") {
            Distro::Fedora
        } else if ids.contains("arch") {
            Distro::Arch
        } else {
            Distro::Other
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CodecStatus {
    pub name: String,
    pub available: bool,
    /// Hardware device types the decoder can use (decoders only).
    pub hw: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceStatus {
    pub kind: String,
    pub device: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub severity: Severity,
    pub what: String,
    pub impact: String,
    pub fix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Some sources can't be opened at all.
    Error,
    /// Works, but slowly (software fallback).
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub distro: Distro,
    pub ffmpeg_version: String,
    pub libraries: BTreeMap<String, String>,
    pub hw_device_types: Vec<String>,
    pub decoders: Vec<CodecStatus>,
    pub encoders: Vec<CodecStatus>,
    pub devices: Vec<DeviceStatus>,
    pub issues: Vec<Issue>,
}

const DECODERS: &[&str] = &["h264", "hevc", "av1", "prores", "aac", "opus", "pcm_s16le"];
const ENCODERS: &[&str] = &[
    "libx264",
    "libx265",
    "libsvtav1",
    "h264_vaapi",
    "hevc_vaapi",
    "av1_vaapi",
    "prores_ks",
    "aac",
];

fn fedora_full_ffmpeg() -> &'static str {
    "sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm\n\
     sudo dnf swap ffmpeg-free ffmpeg --allowerasing"
}

pub fn run(render_node: &Path) -> DoctorReport {
    init();
    let distro = Distro::detect();
    let decoders: Vec<CodecStatus> = DECODERS
        .iter()
        .map(|&n| match ff::decoder::find_by_name(n) {
            Some(c) => CodecStatus {
                name: n.into(),
                available: true,
                hw: ffi::decoder_hw_configs(&c),
            },
            None => CodecStatus {
                name: n.into(),
                available: false,
                hw: vec![],
            },
        })
        .collect();
    let encoders: Vec<CodecStatus> = ENCODERS
        .iter()
        .map(|&n| CodecStatus {
            name: n.into(),
            available: ff::encoder::find_by_name(n).is_some(),
            hw: vec![],
        })
        .collect();
    let node = render_node.to_string_lossy().into_owned();
    let devices = vec![
        device_status("vaapi", Some(&node)),
        device_status("vulkan", None),
    ];
    let hw_device_types = ffi::hw_device_types();
    let mut issues = Vec::new();
    let has = |list: &[CodecStatus], n: &str| list.iter().any(|c| c.name == n && c.available);

    for (codec, what) in [("hevc", "HEVC"), ("h264", "H.264")] {
        if !has(&decoders, codec) {
            issues.push(Issue {
                severity: Severity::Error,
                what: format!("FFmpeg has no {what} decoder"),
                impact: format!("{what} files (most 360 cameras record HEVC) cannot be opened"),
                fix: match distro {
                    Distro::Fedora => fedora_full_ffmpeg().into(),
                    Distro::Arch => "sudo pacman -S --needed ffmpeg".into(),
                    Distro::Other => "install a full FFmpeg build".into(),
                },
            });
        }
    }
    let vaapi = devices.iter().find(|d| d.kind == "vaapi");
    let node_access = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(render_node)
        .map(drop);
    if !vaapi.is_some_and(|d| d.ok)
        && let Err(e) = &node_access
    {
        let others = other_render_nodes(render_node);
        let (what, fix) = match e.kind() {
            std::io::ErrorKind::NotFound => (
                format!("no DRM render node at {node}"),
                if others.is_empty() {
                    "no GPU driver is loaded; in a VM or container, pass the GPU through (for example `--device /dev/dri`)".to_string()
                } else {
                    format!(
                        "this machine has {}; pass it with --render-node",
                        others.join(", ")
                    )
                },
            ),
            std::io::ErrorKind::PermissionDenied => (
                format!("no permission to open {node}"),
                "sudo usermod -aG render \"$USER\"   then log out and back in".to_string(),
            ),
            _ => (
                format!("cannot open {node}: {e}"),
                "check the GPU driver with `vainfo`".to_string(),
            ),
        };
        issues.push(Issue {
            severity: Severity::Warning,
            what,
            impact: "decode and encode fall back to the CPU, which is far slower at 5.7K and up"
                .into(),
            fix,
        });
    } else if !vaapi.is_some_and(|d| d.ok) {
        let amd = gpu_vendor(render_node).as_deref() == Some("0x1002");
        let intel = gpu_vendor(render_node).as_deref() == Some("0x8086");
        issues.push(Issue {
            severity: Severity::Warning,
            what: format!(
                "no VA-API device at {node}{}",
                vaapi
                    .and_then(|d| d.error.as_deref())
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            ),
            impact: "decode and encode fall back to the CPU, which is far slower at 5.7K and up"
                .into(),
            fix: match (distro, amd, intel) {
                (Distro::Fedora, true, _) => "sudo dnf install mesa-va-drivers-freeworld libva-utils   (RPM Fusion free)".into(),
                (Distro::Fedora, _, true) => "sudo dnf install intel-media-driver libva-utils   (RPM Fusion nonfree)".into(),
                (Distro::Arch, _, true) => "sudo pacman -S --needed intel-media-driver libva-utils".into(),
                (Distro::Arch, _, _) => "sudo pacman -S --needed mesa libva-utils".into(),
                _ => "install your GPU's VA-API driver; check that your user is in the `render` group".into(),
            },
        });
    } else if distro == Distro::Fedora
        && gpu_vendor(render_node).as_deref() == Some("0x1002")
        && !rpm_installed("mesa-va-drivers-freeworld")
    {
        issues.push(Issue {
            severity: Severity::Warning,
            what: "Fedora's Mesa VA-API driver has H.264/HEVC disabled".into(),
            impact: "HEVC camera footage decodes on the CPU instead of the GPU".into(),
            fix: if rpm_installed("mesa-va-drivers") {
                "sudo dnf swap mesa-va-drivers mesa-va-drivers-freeworld   (RPM Fusion free)"
            } else {
                "sudo dnf install mesa-va-drivers-freeworld   (RPM Fusion free)"
            }
            .into(),
        });
    }
    if distro == Distro::Fedora && rpm_installed("ffmpeg-free") {
        issues.push(Issue {
            severity: Severity::Warning,
            what: "Fedora's ffmpeg-free is installed".into(),
            impact: "its encoders and VA-API paths leave out H.264/HEVC; exports fall back to slow software or fail".into(),
            fix: fedora_full_ffmpeg().into(),
        });
    }
    for (enc, what) in [("libx265", "HEVC"), ("libx264", "H.264")] {
        if !has(&encoders, enc) {
            issues.push(Issue {
                severity: Severity::Warning,
                what: format!("no software {what} encoder ({enc})"),
                impact: format!("{what} export needs VA-API; there is no CPU fallback"),
                fix: match distro {
                    Distro::Fedora => fedora_full_ffmpeg().into(),
                    Distro::Arch => "sudo pacman -S --needed ffmpeg".into(),
                    Distro::Other => "install FFmpeg with libx264/libx265".into(),
                },
            });
        }
    }
    issues.sort_by_key(|i| i.severity);
    DoctorReport {
        distro,
        ffmpeg_version: ffi::ffmpeg_version(),
        libraries: ffi::lib_versions()
            .into_iter()
            .map(|(n, a, b, c)| (n.to_string(), format!("{a}.{b}.{c}")))
            .collect(),
        hw_device_types,
        decoders,
        encoders,
        devices,
        issues,
    }
}

/// Render nodes other than `render_node`, e.g. on a laptop with two GPUs.
fn other_render_nodes(render_node: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("renderD"))
                && p != render_node
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn device_status(kind: &str, device: Option<&str>) -> DeviceStatus {
    let r = ffi::try_hw_device(kind, device);
    DeviceStatus {
        kind: kind.into(),
        device: device.map(str::to_string),
        ok: r.is_ok(),
        error: r.err().map(|e| e.to_string()),
    }
}

fn gpu_vendor(render_node: &Path) -> Option<String> {
    let name = render_node.file_name()?.to_string_lossy().into_owned();
    std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn rpm_installed(pkg: &str) -> bool {
    std::process::Command::new("rpm")
        .args(["-q", pkg])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_software_codecs_here() {
        // Tier 1: the dev workspace has a full FFmpeg with software codecs.
        let r = run(Path::new("/dev/dri/renderD128"));
        assert!(!r.ffmpeg_version.is_empty());
        assert!(r.libraries.contains_key("libavcodec"));
        let hevc = r.decoders.iter().find(|d| d.name == "hevc").unwrap();
        assert!(hevc.available);
        // The HEVC decoder always advertises VA-API on Linux builds.
        assert!(hevc.hw.iter().any(|h| h == "vaapi"), "{:?}", hevc.hw);
    }
}
