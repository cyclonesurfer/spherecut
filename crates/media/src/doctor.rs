//! `spherecut doctor`: what this machine's FFmpeg, drivers and GPU can
//! actually do, and the exact command that fixes anything missing.
//!
//! Opening a device proves little: Fedora's Mesa VA-API driver opens with
//! H.264 and HEVC turned off, and FFmpeg's Vulkan device opens on
//! llvmpipe, which renders on the CPU. So doctor checks capability. It
//! decodes one tiny embedded frame of each format (H.264 High, HEVC Main
//! and Main 10, AV1 Main; see `fixtures/README.md`) in software, on VA-API
//! and on FFmpeg's Vulkan device, never falling back to software, and
//! requires the hardware's picture to match the software decoder's. It
//! encodes one frame with each VA-API encoder and reads it back, and lists
//! the Vulkan devices FFmpeg sees, with their type and driver.
//!
//! The fixes follow from those results, by distro and GPU vendor. On
//! Fedora, `rpm -q` only chooses between "install" and "swap" wording and
//! whether RPM Fusion needs enabling. Doctor never installs or changes
//! anything.
//!
//! The GUI (from M1) will run the same check at startup, so that a missing
//! HEVC decoder is reported up front, not as a decode error halfway
//! through a project.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::Path;

use serde::Serialize;

use crate::decode::DecodedFrame;
use crate::hwprobe::{self, ENCODE_SIZE, ProbeError, device_label, vk_device_type};
use crate::{HwDevice, HwError, ff, ffi, init};

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

/// GPU vendor, from its PCI vendor ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Amd,
    Intel,
    Other,
    /// No GPU found to ask.
    Unknown,
}

impl Vendor {
    fn from_pci_id(id: u32) -> Self {
        match id {
            0x1002 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            _ => Vendor::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Gpu {
    /// The DRM render node checked for VA-API.
    pub render_node: String,
    /// The render node's GPU vendor (from sysfs), or else the first Vulkan
    /// GPU's.
    pub vendor: Vendor,
    pub vendor_id: Option<String>,
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
    /// The physical device FFmpeg runs on (Vulkan).
    pub name: Option<String>,
    /// FFmpeg's warnings and errors while opening it.
    pub ffmpeg_log: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VulkanDeviceType {
    IntegratedGpu,
    DiscreteGpu,
    VirtualGpu,
    /// A software rasterizer such as llvmpipe: no GPU acceleration.
    Cpu,
    Other,
}

/// A Vulkan physical device of the instance behind FFmpeg's Vulkan device.
#[derive(Debug, Clone, Serialize)]
pub struct VulkanDevice {
    pub name: String,
    pub device_type: VulkanDeviceType,
    pub vendor_id: String,
    pub device_id: String,
    pub api_version: String,
    /// The driver's name, e.g. "radv" or "llvmpipe" (Vulkan 1.2 and up).
    pub driver: Option<String>,
    /// e.g. "Mesa 25.2.8".
    pub driver_info: Option<String>,
    pub used_by_ffmpeg: bool,
}

impl VulkanDevice {
    /// Whether it renders on the CPU, i.e. gives no GPU acceleration.
    pub fn is_cpu(&self) -> bool {
        self.device_type == VulkanDeviceType::Cpu
    }
}

impl From<hwprobe::VkDevice> for VulkanDevice {
    fn from(d: hwprobe::VkDevice) -> Self {
        let (major, minor, patch) = hwprobe::vk_version(d.api_version);
        VulkanDevice {
            device_type: match d.device_type {
                vk_device_type::INTEGRATED_GPU => VulkanDeviceType::IntegratedGpu,
                vk_device_type::DISCRETE_GPU => VulkanDeviceType::DiscreteGpu,
                vk_device_type::VIRTUAL_GPU => VulkanDeviceType::VirtualGpu,
                vk_device_type::CPU => VulkanDeviceType::Cpu,
                _ => VulkanDeviceType::Other,
            },
            name: d.name,
            vendor_id: format!("{:#06x}", d.vendor_id),
            device_id: format!("{:#06x}", d.device_id),
            api_version: format!("{major}.{minor}.{patch}"),
            driver: d.driver_name,
            driver_info: d.driver_info,
            used_by_ffmpeg: d.ffmpeg_uses,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Works,
    Fails,
    /// The device isn't there to test.
    NotTested,
}

/// What a failed check lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fault {
    /// This FFmpeg build: a decoder, encoder or hwaccel left out.
    Ffmpeg,
    /// The driver or the hardware refuses the format, or fails on it.
    Driver,
    /// The driver runs it and gets it wrong: a picture that differs from
    /// the software decoder's or has the wrong size or format, or an
    /// encoding that doesn't read back as it should.
    WrongOutput,
}

/// One decode or encode of one format on one path.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// e.g. "HEVC Main 10".
    pub format: String,
    /// "software", "vaapi" or "vulkan".
    pub path: String,
    /// The FFmpeg decoder or encoder used, e.g. "hevc" or "hevc_vaapi".
    pub codec: String,
    pub result: CheckResult,
    /// Why it fails or wasn't tested, or a note on how it works.
    pub reason: Option<String>,
    pub fault: Option<Fault>,
    /// FFmpeg's warnings and errors during the check.
    pub ffmpeg_log: Vec<String>,
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
    /// Sources open, but something is slow (a software fallback), missing
    /// (such as an export or audio codec) or wrong (a driver's output).
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
    pub gpu: Gpu,
    pub devices: Vec<DeviceStatus>,
    pub vulkan_devices: Vec<VulkanDevice>,
    /// Each format decoded in software, on VA-API and on Vulkan.
    pub decode_checks: Vec<Check>,
    /// Each VA-API encoder, one frame each.
    pub encode_checks: Vec<Check>,
    pub issues: Vec<Issue>,
}

/// A decoder or encoder doctor looks for, and the issue when FFmpeg lacks
/// it.
struct Need {
    codec: &'static str,
    severity: Severity,
    /// What it is, e.g. "HEVC decoder".
    what: &'static str,
    /// What is lost without it.
    impact: &'static str,
}

const fn need(
    codec: &'static str,
    severity: Severity,
    what: &'static str,
    impact: &'static str,
) -> Need {
    Need {
        codec,
        severity,
        what,
        impact,
    }
}

const DECODERS: &[Need] = &[
    need(
        "h264",
        Severity::Error,
        "H.264 decoder",
        "H.264 files cannot be opened",
    ),
    need(
        "hevc",
        Severity::Error,
        "HEVC decoder",
        "HEVC files (most 360 cameras record HEVC) cannot be opened",
    ),
    need(
        "av1",
        Severity::Info,
        "AV1 decoder for VA-API and Vulkan Video",
        "AV1 cannot decode on the GPU",
    ),
    // Only when the software AV1 check works without it; when that check
    // fails, its own issue names libdav1d.
    need(
        LIBDAV1D,
        Severity::Info,
        "dav1d AV1 decoder",
        "AV1 decodes in software with libaom-av1, which is far slower",
    ),
    need(
        "prores",
        Severity::Error,
        "ProRes decoder",
        "ProRes files cannot be opened",
    ),
    need(
        "aac",
        Severity::Error,
        "AAC decoder",
        "the audio of nearly every camera file cannot be decoded",
    ),
    need(
        "opus",
        Severity::Warning,
        "Opus decoder",
        "Opus audio cannot be decoded",
    ),
    need(
        "pcm_s16le",
        Severity::Warning,
        "16-bit PCM decoder",
        "16-bit PCM audio cannot be decoded",
    ),
];

// FFmpeg's VA-API encoders are `<codec>_vaapi`. A build without VA-API has
// none, and the issue that says so covers them.
const ENCODERS: &[Need] = &[
    need(
        "libx264",
        Severity::Warning,
        "software H.264 encoder",
        "H.264 export needs VA-API; there is no CPU fallback",
    ),
    need(
        "libx265",
        Severity::Warning,
        "software HEVC encoder",
        "HEVC export needs VA-API; there is no CPU fallback",
    ),
    need(
        "libsvtav1",
        Severity::Warning,
        "software AV1 encoder",
        "AV1 export needs VA-API; there is no CPU fallback",
    ),
    need(
        "h264_vaapi",
        Severity::Warning,
        "VA-API H.264 encoder",
        "H.264 export can't use the GPU: it runs on the CPU, which is far slower at 5.7K and up",
    ),
    need(
        "hevc_vaapi",
        Severity::Warning,
        "VA-API HEVC encoder",
        "HEVC export can't use the GPU: it runs on the CPU, which is far slower at 5.7K and up",
    ),
    need(
        "av1_vaapi",
        Severity::Info,
        "VA-API AV1 encoder",
        "AV1 export can't use the GPU",
    ),
    need(
        "prores_ks",
        Severity::Info,
        "ProRes encoder",
        "no ProRes export",
    ),
    need(
        "aac",
        Severity::Warning,
        "AAC encoder",
        "exports cannot have AAC audio",
    ),
];

const LIBDAV1D: &str = "libdav1d";

pub const SOFTWARE: &str = "software";
const AV1: &str = "AV1 Main";

/// One frame of one format, embedded from `fixtures/`.
struct Sample {
    format: &'static str,
    data: &'static [u8],
    /// Software decoders, best first.
    software: &'static [&'static str],
    /// What the software decoders output.
    pix_fmt: &'static str,
    /// The decoder that runs on hardware devices.
    hardware: &'static str,
}

/// Width and height of the embedded frames.
pub const SAMPLE_SIZE: u32 = 256;

const SAMPLES: &[Sample] = &[
    Sample {
        format: "H.264 High",
        data: include_bytes!("../fixtures/h264-high.h264"),
        software: &["h264"],
        pix_fmt: "yuv420p",
        hardware: "h264",
    },
    Sample {
        format: "HEVC Main",
        data: include_bytes!("../fixtures/hevc-main.hevc"),
        software: &["hevc"],
        pix_fmt: "yuv420p",
        hardware: "hevc",
    },
    Sample {
        format: "HEVC Main 10",
        data: include_bytes!("../fixtures/hevc-main10.hevc"),
        software: &["hevc"],
        pix_fmt: "yuv420p10le",
        hardware: "hevc",
    },
    // FFmpeg's own AV1 decoder only drives hardware; software AV1 is
    // dav1d (or libaom).
    Sample {
        format: AV1,
        data: include_bytes!("../fixtures/av1-main.obu"),
        software: &["libdav1d", "libaom-av1"],
        pix_fmt: "yuv420p",
        hardware: "av1",
    },
];

/// One VA-API encoder to try, and the software decoders that read its
/// output back.
struct EncodeTarget {
    format: &'static str,
    encoder: &'static str,
    ten_bit: bool,
    read_back: &'static [&'static str],
}

const ENCODE_TARGETS: &[EncodeTarget] = &[
    EncodeTarget {
        format: "H.264",
        encoder: "h264_vaapi",
        ten_bit: false,
        read_back: &["h264"],
    },
    EncodeTarget {
        format: "HEVC Main",
        encoder: "hevc_vaapi",
        ten_bit: false,
        read_back: &["hevc"],
    },
    EncodeTarget {
        format: "HEVC Main 10",
        encoder: "hevc_vaapi",
        ten_bit: true,
        read_back: &["hevc"],
    },
    EncodeTarget {
        format: "AV1",
        encoder: "av1_vaapi",
        ten_bit: false,
        read_back: &["libdav1d", "libaom-av1"],
    },
];

/// Fedora packages whose presence changes the wording of a fix.
const FEDORA_PACKAGES: &[&str] = &[
    "rpmfusion-free-release",
    "rpmfusion-nonfree-release",
    "ffmpeg-free",
    "mesa-va-drivers",
    "mesa-va-drivers-freeworld",
    "libva-intel-media-driver",
    "intel-media-driver",
    "mesa-vulkan-drivers",
];

pub fn run(render_node: &Path) -> DoctorReport {
    init();
    let distro = Distro::detect();
    let decoders: Vec<CodecStatus> = DECODERS
        .iter()
        .map(|n| match ff::decoder::find_by_name(n.codec) {
            Some(c) => CodecStatus {
                name: n.codec.into(),
                available: true,
                hw: ffi::decoder_hw_configs(&c),
            },
            None => CodecStatus {
                name: n.codec.into(),
                available: false,
                hw: vec![],
            },
        })
        .collect();
    let encoders: Vec<CodecStatus> = ENCODERS
        .iter()
        .map(|n| CodecStatus {
            name: n.codec.into(),
            available: ff::encoder::find_by_name(n.codec).is_some(),
            hw: vec![],
        })
        .collect();
    let node = render_node.to_string_lossy().into_owned();
    let hw_device_types = ffi::hw_device_types();
    let (vaapi, vaapi_status) = open_device("vaapi", Some(&node), &hw_device_types);
    let (vulkan, mut vulkan_status) = open_device("vulkan", None, &hw_device_types);
    let (vulkan_devices, vulkan_error) = match vulkan.as_ref().map(hwprobe::vulkan_devices) {
        Some(Ok(v)) => (v.into_iter().map(VulkanDevice::from).collect(), None),
        Some(Err(e)) => (Vec::new(), Some(e.to_string())),
        None => (Vec::new(), None),
    };
    vulkan_status.name = vulkan_devices
        .iter()
        .find(|d| d.used_by_ffmpeg)
        .map(|d| d.name.clone());
    let paths = [
        ("vaapi", vaapi.as_ref(), &vaapi_status),
        ("vulkan", vulkan.as_ref(), &vulkan_status),
    ];
    let decode_checks = SAMPLES.iter().flat_map(|s| decode(s, &paths)).collect();
    let encode_checks = ENCODE_TARGETS
        .iter()
        .map(|t| encode(t, vaapi.as_ref(), &vaapi_status))
        .collect();
    let gpu = Gpu {
        render_node: node,
        ..gpu_vendor(render_node, &vulkan_devices)
    };
    let env = Env::detect(render_node, distro, vulkan_error);
    let mut report = DoctorReport {
        distro,
        ffmpeg_version: ffi::ffmpeg_version(),
        libraries: ffi::lib_versions()
            .into_iter()
            .map(|(n, a, b, c)| (n.to_string(), format!("{a}.{b}.{c}")))
            .collect(),
        hw_device_types,
        decoders,
        encoders,
        gpu,
        devices: vec![vaapi_status, vulkan_status],
        vulkan_devices,
        decode_checks,
        encode_checks,
        issues: Vec::new(),
    };
    report.issues = issues(&report, &env);
    report
}

fn open_device(
    kind: &str,
    device: Option<&str>,
    built_in: &[String],
) -> (Option<HwDevice>, DeviceStatus) {
    // FFmpeg knows every device type's name, compiled in or not, and fails
    // to create one it lacks with ENOMEM.
    let (r, ffmpeg_log) = if built_in.iter().any(|t| t == kind) {
        hwprobe::capture_ffmpeg_log(|| HwDevice::open(kind, device))
    } else {
        (
            Err(HwError::NotBuiltIn(device_label(kind).into())),
            Vec::new(),
        )
    };
    let status = DeviceStatus {
        kind: kind.into(),
        device: device.map(str::to_string),
        ok: r.is_ok(),
        error: r.as_ref().err().map(|e| e.to_string()),
        name: None,
        ffmpeg_log,
    };
    (r.ok(), status)
}

/// Why a check failed, from a probe error.
struct Failure {
    reason: String,
    fault: Fault,
}

impl From<ProbeError> for Failure {
    fn from(e: ProbeError) -> Self {
        Failure {
            fault: if e.is_ffmpeg_build() {
                Fault::Ffmpeg
            } else {
                Fault::Driver
            },
            reason: e.to_string(),
        }
    }
}

impl Check {
    fn new(format: &str, path: &str, codec: &str, result: CheckResult) -> Self {
        Check {
            format: format.into(),
            path: path.into(),
            codec: codec.into(),
            result,
            reason: None,
            fault: None,
            ffmpeg_log: Vec::new(),
        }
    }

    fn outcome(mut self, r: Result<Option<String>, Failure>, log: Vec<String>) -> Self {
        self.ffmpeg_log = log;
        match r {
            Ok(note) => self.reason = note,
            Err(f) => {
                self.result = CheckResult::Fails;
                self.reason = Some(f.reason);
                self.fault = Some(f.fault);
            }
        }
        self
    }

    fn not_tested(format: &str, path: &str, codec: &str, status: &DeviceStatus) -> Self {
        let mut c = Check::new(format, path, codec, CheckResult::NotTested);
        c.reason = Some(format!(
            "no {} device ({})",
            device_label(path),
            status.error.as_deref().unwrap_or("not opened")
        ));
        c
    }
}

fn first_decoder<'a>(names: &[&'a str]) -> Option<&'a str> {
    names
        .iter()
        .copied()
        .find(|n| ff::decoder::find_by_name(n).is_some())
}

/// The planes of a decoded `size`² 4:2:0 frame, or what's wrong with it,
/// blamed on `fault`.
fn planes(f: &DecodedFrame, size: u32, fault: Fault) -> Result<[Vec<u16>; 3], Failure> {
    let got = (f.frame.width(), f.frame.height());
    let fail = |reason| Failure { reason, fault };
    if got != (size, size) {
        return Err(fail(format!(
            "decodes to {}x{} instead of {size}x{size}",
            got.0, got.1
        )));
    }
    f.planes_u16().ok_or_else(|| {
        let fmt = ffi::frame_format(&f.frame);
        fail(format!(
            "decodes to unexpected {} frames",
            ffi::pix_fmt_name(fmt).unwrap_or_else(|| format!("format {fmt}"))
        ))
    })
}

/// Decode `s` in software and on each device path. The software picture
/// is the reference: H.264, HEVC and AV1 decoding is exactly specified, so
/// a device whose picture differs has a broken driver.
fn decode(s: &Sample, paths: &[(&str, Option<&HwDevice>, &DeviceStatus)]) -> Vec<Check> {
    let (software, reference) = decode_in_software(s);
    let mut out = vec![software];
    for &(kind, dev, status) in paths {
        let Some(dev) = dev else {
            out.push(Check::not_tested(s.format, kind, s.hardware, status));
            continue;
        };
        let (r, log) =
            hwprobe::capture_ffmpeg_log(|| hwprobe::decode_one(s.hardware, s.data, Some(dev)));
        let r = r
            .map_err(Failure::from)
            .and_then(|f| planes(&f, SAMPLE_SIZE, Fault::WrongOutput))
            .and_then(|got| match &reference {
                Some(want) => compare(want, &got),
                None => Ok(Some("not compared: no software decode".into())),
            });
        out.push(Check::new(s.format, kind, s.hardware, CheckResult::Works).outcome(r, log));
    }
    out
}

/// The software check of `s`, and the picture if it works.
fn decode_in_software(s: &Sample) -> (Check, Option<[Vec<u16>; 3]>) {
    let Some(name) = first_decoder(s.software) else {
        let e = ProbeError::NoDecoder(s.software.join(" or "));
        let check = Check::new(s.format, SOFTWARE, s.software[0], CheckResult::Works);
        return (check.outcome(Err(e.into()), Vec::new()), None);
    };
    let check = Check::new(s.format, SOFTWARE, name, CheckResult::Works);
    let (r, log) = hwprobe::capture_ffmpeg_log(|| hwprobe::decode_one(name, s.data, None));
    let r = r
        .map_err(|e| Failure {
            fault: Fault::Ffmpeg,
            ..e.into()
        })
        .and_then(|f| {
            let fmt = ffi::pix_fmt_name(ffi::frame_format(&f.frame));
            if fmt.as_deref() != Some(s.pix_fmt) {
                return Err(Failure {
                    reason: format!(
                        "decodes to {} frames, not {}",
                        fmt.as_deref().unwrap_or("unknown"),
                        s.pix_fmt
                    ),
                    fault: Fault::Ffmpeg,
                });
            }
            planes(&f, SAMPLE_SIZE, Fault::Ffmpeg)
        });
    match r {
        Ok(p) => (check.outcome(Ok(None), log), Some(p)),
        Err(f) => (check.outcome(Err(f), log), None),
    }
}

fn compare(want: &[Vec<u16>; 3], got: &[Vec<u16>; 3]) -> Result<Option<String>, Failure> {
    let total: usize = want.iter().map(Vec::len).sum();
    let differ: usize = want
        .iter()
        .zip(got)
        .map(|(a, b)| a.iter().zip(b).filter(|(x, y)| x != y).count())
        .sum();
    if differ == 0 {
        Ok(None)
    } else {
        Err(Failure {
            reason: format!("{differ} of {total} samples differ from FFmpeg's software decoder"),
            fault: Fault::WrongOutput,
        })
    }
}

/// Encode one frame with `t` on the VA-API device, then decode it in
/// software: the encoder must produce a real stream of the right size and
/// bit depth.
fn encode(t: &EncodeTarget, dev: Option<&HwDevice>, status: &DeviceStatus) -> Check {
    let Some(dev) = dev else {
        return Check::not_tested(t.format, "vaapi", t.encoder, status);
    };
    let check = Check::new(t.format, "vaapi", t.encoder, CheckResult::Works);
    let (r, mut log) =
        hwprobe::capture_ffmpeg_log(|| hwprobe::encode_one(t.encoder, dev, t.ten_bit));
    let bytes = match r {
        Ok(b) => b,
        Err(e) => return check.outcome(Err(e.into()), log),
    };
    let Some(decoder) = first_decoder(t.read_back) else {
        let note = format!("{} bytes; not read back: no software decoder", bytes.len());
        return check.outcome(Ok(Some(note)), log);
    };
    let (r, more) = hwprobe::capture_ffmpeg_log(|| hwprobe::decode_one(decoder, &bytes, None));
    log.extend(more);
    let want = if t.ten_bit { "yuv420p10le" } else { "yuv420p" };
    let r = match r {
        Err(e) => Err(Failure {
            reason: format!("its output doesn't decode: {e}"),
            fault: Fault::WrongOutput,
        }),
        Ok(f) => {
            let fmt = ffi::pix_fmt_name(ffi::frame_format(&f.frame));
            let size = (f.frame.width(), f.frame.height());
            if size != (ENCODE_SIZE, ENCODE_SIZE) || fmt.as_deref() != Some(want) {
                Err(Failure {
                    reason: format!(
                        "its output reads back as {}x{} {}, not {ENCODE_SIZE}x{ENCODE_SIZE} {want}",
                        size.0,
                        size.1,
                        fmt.as_deref().unwrap_or("?")
                    ),
                    fault: Fault::WrongOutput,
                })
            } else {
                Ok(None)
            }
        }
    };
    check.outcome(r, log)
}

/// The render node's vendor from sysfs, else the first Vulkan GPU's.
fn gpu_vendor(render_node: &Path, vulkan: &[VulkanDevice]) -> Gpu {
    let sysfs = render_node.file_name().and_then(|name| {
        let p = format!("/sys/class/drm/{}/device/vendor", name.to_string_lossy());
        std::fs::read_to_string(p).ok()
    });
    let id = sysfs
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .or_else(|| {
            vulkan
                .iter()
                .find(|d| !d.is_cpu())
                .and_then(|d| u32::from_str_radix(d.vendor_id.trim_start_matches("0x"), 16).ok())
        });
    Gpu {
        render_node: String::new(),
        vendor: id.map_or(Vendor::Unknown, Vendor::from_pci_id),
        vendor_id: id.map(|i| format!("{i:#06x}")),
    }
}

/// What the issues depend on besides the report.
#[derive(Debug, Default)]
struct Env {
    /// Why the render node can't be opened read-write.
    node_error: Option<(ErrorKind, String)>,
    /// Render nodes other than the one checked, e.g. a second GPU.
    other_nodes: Vec<String>,
    /// Which of [`FEDORA_PACKAGES`] are installed (Fedora only).
    installed: BTreeSet<String>,
    /// Why the Vulkan devices couldn't be listed.
    vulkan_error: Option<String>,
}

impl Env {
    fn detect(render_node: &Path, distro: Distro, vulkan_error: Option<String>) -> Self {
        let node_error = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(render_node)
            .err()
            .map(|e| (e.kind(), e.to_string()));
        Env {
            node_error,
            other_nodes: other_render_nodes(render_node),
            installed: if distro == Distro::Fedora {
                rpm_installed(FEDORA_PACKAGES)
            } else {
                BTreeSet::new()
            },
            vulkan_error,
        }
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

/// Which of `pkgs` are installed, by one `rpm -q` query.
fn rpm_installed(pkgs: &[&str]) -> BTreeSet<String> {
    let out = std::process::Command::new("rpm")
        .args(["-q", "--qf", "%{NAME}\\n"])
        .args(pkgs)
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return BTreeSet::new();
    };
    // Installed packages print their name; others "package … is not
    // installed".
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| pkgs.contains(l))
        .map(str::to_string)
        .collect()
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Fix commands for this distro and GPU.
struct Fixes<'a> {
    distro: Distro,
    vendor: Vendor,
    installed: &'a BTreeSet<String>,
    /// No render node at all: probably a VM or container.
    no_gpu_visible: bool,
    /// The render node is there, and we may not open it.
    node_denied: bool,
}

impl Fixes<'_> {
    fn has(&self, pkg: &str) -> bool {
        self.installed.contains(pkg)
    }

    /// The command that enables RPM Fusion's `repo` ("free" or
    /// "nonfree"), unless it already is.
    fn rpmfusion(&self, repo: &str) -> Option<String> {
        (!self.has(&format!("rpmfusion-{repo}-release"))).then(|| {
            format!(
                "sudo dnf install https://mirrors.rpmfusion.org/{repo}/fedora/\
                 rpmfusion-{repo}-release-$(rpm -E %fedora).noarch.rpm"
            )
        })
    }

    /// An AMD and an Intel fix, or the one for this GPU.
    fn by_vendor(&self, amd: impl Fn() -> String, intel: impl Fn() -> String) -> String {
        match self.vendor {
            Vendor::Amd => amd(),
            Vendor::Intel => intel(),
            _ => format!("# AMD GPU:\n{}\n# Intel GPU:\n{}", amd(), intel()),
        }
    }

    /// A full FFmpeg: H.264/HEVC, dav1d, libx264/libx265, SVT-AV1, VA-API,
    /// Vulkan.
    fn ffmpeg(&self) -> String {
        match self.distro {
            Distro::Fedora => {
                let cmd = if self.has("ffmpeg-free") {
                    "sudo dnf swap ffmpeg-free ffmpeg --allowerasing"
                } else {
                    "sudo dnf install ffmpeg --allowerasing"
                };
                lines([
                    self.rpmfusion("free"),
                    Some(format!("{cmd}   (RPM Fusion's full FFmpeg)")),
                ])
            }
            Distro::Arch => "sudo pacman -S --needed ffmpeg".into(),
            Distro::Other => "install a full FFmpeg build: H.264/HEVC, libdav1d, \
                              libx264/libx265, SVT-AV1, VA-API and Vulkan"
                .into(),
        }
    }

    /// Fedora: Mesa's VA-API driver with H.264 and HEVC, from RPM Fusion.
    fn fedora_mesa_va(&self) -> String {
        let cmd = if self.has("mesa-va-drivers") {
            "sudo dnf swap mesa-va-drivers mesa-va-drivers-freeworld"
        } else {
            "sudo dnf install mesa-va-drivers-freeworld"
        };
        lines([
            self.rpmfusion("free"),
            Some(format!("{cmd}   (RPM Fusion free)")),
        ])
    }

    /// Fedora: Intel's full VA-API driver, from RPM Fusion.
    fn fedora_intel_va(&self) -> String {
        let cmd = if self.has("libva-intel-media-driver") {
            "sudo dnf swap libva-intel-media-driver intel-media-driver --allowerasing"
        } else {
            "sudo dnf install intel-media-driver"
        };
        lines([
            self.rpmfusion("nonfree"),
            Some(format!("{cmd}   (RPM Fusion nonfree)")),
        ])
    }

    /// A VA-API driver, for a render node where none opens.
    fn vaapi_driver(&self) -> String {
        self.by_vendor(
            || match self.distro {
                Distro::Fedora => self.fedora_mesa_va(),
                Distro::Arch => "sudo pacman -S --needed mesa libva-utils".into(),
                Distro::Other => "install Mesa's VA-API driver (radeonsi)".into(),
            },
            || match self.distro {
                Distro::Fedora => self.fedora_intel_va(),
                Distro::Arch => "sudo pacman -S --needed intel-media-driver libva-utils".into(),
                Distro::Other => "install Intel's VA-API driver (intel-media-driver, iHD)".into(),
            },
        )
    }

    /// H.264/HEVC for a VA-API driver that opens without them.
    fn vaapi_codecs(&self, node: &str) -> String {
        let vainfo = vainfo(node);
        self.by_vendor(
            || match self.distro {
                Distro::Fedora if !self.has("mesa-va-drivers-freeworld") => self.fedora_mesa_va(),
                Distro::Fedora => format!(
                    "sudo dnf upgrade --refresh   \
                     (mesa-va-drivers-freeworld must match the installed Mesa)\n{vainfo}"
                ),
                Distro::Arch => {
                    format!("sudo pacman -Syu   (Arch's Mesa has H.264 and HEVC)\n{vainfo}")
                }
                Distro::Other => "install a Mesa VA-API driver built with \
                                  video-codecs=h264dec,h264enc,h265dec,h265enc"
                    .into(),
            },
            || match self.distro {
                Distro::Fedora => self.fedora_intel_va(),
                Distro::Arch => "sudo pacman -S --needed intel-media-driver\n\
                                 export LIBVA_DRIVER_NAME=iHD   \
                                 (if `vainfo` shows the old i965 driver)"
                    .into(),
                Distro::Other => {
                    "install intel-media-driver and use it: export LIBVA_DRIVER_NAME=iHD".into()
                }
            },
        )
    }

    /// A Vulkan driver for the GPU.
    fn vulkan_driver(&self) -> String {
        let driver = match self.distro {
            Distro::Fedora => {
                "sudo dnf install mesa-vulkan-drivers vulkan-loader   (RADV for AMD, ANV for Intel)"
                    .into()
            }
            Distro::Arch => self.by_vendor(
                || "sudo pacman -S --needed vulkan-radeon vulkan-icd-loader".into(),
                || "sudo pacman -S --needed vulkan-intel vulkan-icd-loader".into(),
            ),
            Distro::Other => {
                "install Mesa's Vulkan driver for your GPU (RADV for AMD, ANV for Intel)".into()
            }
        };
        lines([
            self.no_gpu_visible.then(|| PASS_THROUGH.to_string()),
            Some(driver),
        ])
    }

    /// A newer `driver` (e.g. "radv (Mesa 25.2.8)"), for one that runs a
    /// format and gets it wrong.
    fn newer_driver(&self, driver: &str) -> String {
        let cmd = match self.distro {
            Distro::Fedora => "sudo dnf upgrade --refresh",
            Distro::Arch => "sudo pacman -Syu",
            Distro::Other => "update the GPU driver",
        };
        format!(
            "{cmd}   ({driver} gets this wrong: a newer release may fix it; if not, \
             report it to its developers)"
        )
    }
}

impl Fixes<'_> {
    /// Vulkan on the GPU, where only a CPU device shows up.
    fn vulkan_gpu(&self) -> String {
        if self.node_denied {
            return format!("{ADD_TO_RENDER}   (Vulkan opens the GPU through its render node)");
        }
        // A CPU device was listed, so the Vulkan loader is there, and with
        // Mesa's drivers installed too nothing is left to install.
        let mesa = self.distro == Distro::Fedora && self.has("mesa-vulkan-drivers");
        // No driver loads for a GPU that isn't passed through.
        if self.no_gpu_visible {
            return if mesa {
                PASS_THROUGH.into()
            } else {
                self.vulkan_driver()
            };
        }
        if mesa {
            return "VK_LOADER_DEBUG=error,warn vulkaninfo --summary   (Mesa's Vulkan drivers \
                    are installed: this shows why none loads for the GPU)\n\
                    sudo dnf upgrade --refresh   (a GPU newer than its Mesa needs a newer Mesa)"
                .into();
        }
        self.vulkan_driver()
    }
}

const ADD_TO_RENDER: &str = "sudo usermod -aG render \"$USER\"   then log out and back in";

const PASS_THROUGH: &str = "no GPU is visible here: in a VM or container, pass it through \
                            first (for example `--device /dev/dri`)";

fn vainfo(node: &str) -> String {
    format!("vainfo --display drm --device {node}   (lists what this GPU decodes and encodes)")
}

fn lines(parts: impl IntoIterator<Item = Option<String>>) -> String {
    parts.into_iter().flatten().collect::<Vec<_>>().join("\n")
}

const CPU_FALLBACK: &str =
    "decode and encode fall back to the CPU, which is far slower at 5.7K and up";

fn issues(r: &DoctorReport, env: &Env) -> Vec<Issue> {
    let fx = Fixes {
        distro: r.distro,
        vendor: r.gpu.vendor,
        installed: &env.installed,
        no_gpu_visible: env
            .node_error
            .as_ref()
            .is_some_and(|(k, _)| *k == ErrorKind::NotFound)
            && env.other_nodes.is_empty(),
        node_denied: env
            .node_error
            .as_ref()
            .is_some_and(|(k, _)| *k == ErrorKind::PermissionDenied),
    };
    let mut out = Vec::new();
    let av1_software_fails = r
        .decode_checks
        .iter()
        .any(|c| c.format == AV1 && c.path == SOFTWARE && c.result != CheckResult::Works);
    let vaapi_built_in = r.hw_device_types.iter().any(|t| t == "vaapi");
    for (needs, codecs) in [(DECODERS, &r.decoders), (ENCODERS, &r.encoders)] {
        for n in needs {
            let missing = codecs.iter().any(|c| c.name == n.codec && !c.available);
            let covered = (n.codec == LIBDAV1D && av1_software_fails)
                || (n.codec.ends_with("_vaapi") && !vaapi_built_in);
            if missing && !covered {
                out.push(Issue {
                    severity: n.severity,
                    what: format!("FFmpeg has no {} ({})", n.what, n.codec),
                    impact: n.impact.into(),
                    fix: fx.ffmpeg(),
                });
            }
        }
    }
    if av1_software_fails {
        let on_gpu = r
            .decode_checks
            .iter()
            .any(|c| c.format == AV1 && c.path != SOFTWARE && c.result == CheckResult::Works);
        out.push(Issue {
            severity: if on_gpu {
                Severity::Warning
            } else {
                Severity::Error
            },
            what: "FFmpeg has no working software AV1 decoder (libdav1d)".into(),
            impact: if on_gpu {
                "AV1 files decode only on the GPU, with no fallback".into()
            } else {
                "AV1 files cannot be opened".into()
            },
            fix: fx.ffmpeg(),
        });
    }
    vaapi_issues(r, env, &fx, &mut out);
    vulkan_issues(r, env, &fx, &mut out);
    out.sort_by_key(|i| i.severity);
    out
}

/// Checks on `path` that failed for `fault`. A check whose decoder or
/// encoder is missing from `codecs` is left out: the missing codec has an
/// issue of its own.
fn failures<'a>(
    checks: &'a [Check],
    codecs: &[CodecStatus],
    path: &str,
    fault: Fault,
) -> Vec<&'a Check> {
    let missing = |name: &str| codecs.iter().any(|c| c.name == name && !c.available);
    checks
        .iter()
        .filter(|c| {
            c.path == path
                && c.result == CheckResult::Fails
                && c.fault == Some(fault)
                && !missing(&c.codec)
        })
        .collect()
}

fn formats<'a>(checks: &[&'a Check]) -> Vec<&'a str> {
    checks.iter().map(|c| c.format.as_str()).collect()
}

/// The formats of [`failures`].
fn failed<'a>(
    checks: &'a [Check],
    codecs: &[CodecStatus],
    path: &str,
    fault: Fault,
) -> Vec<&'a str> {
    formats(&failures(checks, codecs, path, fault))
}

/// "decodes X wrongly (why)", with each format's reason when there are
/// several.
fn wrongly(verb: &str, checks: &[&Check]) -> String {
    let reason = |c: &Check| c.reason.clone().unwrap_or_default();
    let why = match checks {
        [one] => reason(one),
        _ => checks
            .iter()
            .map(|c| format!("{}: {}", c.format, reason(c)))
            .collect::<Vec<_>>()
            .join("; "),
    };
    format!("{verb} {} wrongly ({why})", and_list(&formats(checks)))
}

/// "decode X and Y, or encode Z".
fn cannot(decode: &[&str], encode: &[&str]) -> String {
    match (decode.is_empty(), encode.is_empty()) {
        (false, false) => format!(
            "decode {}, or encode {}",
            and_list(decode),
            and_list(encode)
        ),
        (false, true) => format!("decode {}", and_list(decode)),
        _ => format!("encode {}", and_list(encode)),
    }
}

fn vaapi_issues(r: &DoctorReport, env: &Env, fx: &Fixes<'_>, out: &mut Vec<Issue>) {
    let Some(dev) = r.devices.iter().find(|d| d.kind == "vaapi") else {
        return;
    };
    let node = &r.gpu.render_node;
    let built_in = r.hw_device_types.iter().any(|t| t == "vaapi");
    if !built_in {
        out.push(Issue {
            severity: Severity::Warning,
            what: "this FFmpeg was built without VA-API".into(),
            impact: CPU_FALLBACK.into(),
            fix: fx.ffmpeg(),
        });
    }

    // What this FFmpeg build can't do on VA-API, whatever the driver: from
    // the checks when the device opened, else from the decoders' hwaccels.
    // Missing VA-API encoders have issues of their own.
    let (ffmpeg_dec, ffmpeg_enc): (Vec<&str>, Vec<&str>) = if !built_in {
        (Vec::new(), Vec::new())
    } else if dev.ok {
        (
            failed(&r.decode_checks, &r.decoders, "vaapi", Fault::Ffmpeg),
            failed(&r.encode_checks, &r.encoders, "vaapi", Fault::Ffmpeg),
        )
    } else {
        let no_hwaccel = |n: &str| {
            r.decoders
                .iter()
                .any(|c| c.name == n && c.available && !c.hw.iter().any(|h| h == "vaapi"))
        };
        (
            [("h264", "H.264"), ("hevc", "HEVC")]
                .into_iter()
                .filter(|(n, _)| no_hwaccel(n))
                .map(|(_, w)| w)
                .collect(),
            Vec::new(),
        )
    };
    if !ffmpeg_dec.is_empty() || !ffmpeg_enc.is_empty() {
        out.push(Issue {
            severity: Severity::Warning,
            what: format!(
                "this FFmpeg cannot use VA-API to {}",
                cannot(&ffmpeg_dec, &ffmpeg_enc)
            ),
            impact: CPU_FALLBACK.into(),
            fix: fx.ffmpeg(),
        });
    }

    if !dev.ok {
        let (what, fix) = match &env.node_error {
            // Only the FFmpeg fix above helps until then.
            None if !built_in => return,
            Some((ErrorKind::NotFound, _)) => (
                format!("no DRM render node at {node}"),
                if env.other_nodes.is_empty() {
                    "no GPU driver is loaded; in a VM or container, pass the GPU through \
                     (for example `--device /dev/dri`)"
                        .to_string()
                } else {
                    format!(
                        "this machine has {}; pass it with --render-node",
                        env.other_nodes.join(", ")
                    )
                },
            ),
            Some((ErrorKind::PermissionDenied, _)) => (
                format!("no permission to open {node}"),
                ADD_TO_RENDER.to_string(),
            ),
            Some((_, e)) => (
                format!("cannot open {node}: {e}"),
                "check the GPU driver with `vainfo`".to_string(),
            ),
            None => (
                format!(
                    "no VA-API driver for {node}{}",
                    dev.error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                ),
                fx.vaapi_driver(),
            ),
        };
        out.push(Issue {
            severity: Severity::Warning,
            what,
            impact: CPU_FALLBACK.into(),
            fix,
        });
        return;
    }

    // The device opened: what its driver can't do.
    let (av1_dec, dec): (Vec<&str>, Vec<&str>) =
        failed(&r.decode_checks, &r.decoders, "vaapi", Fault::Driver)
            .into_iter()
            .partition(|f| *f == AV1);
    let (av1_enc, enc): (Vec<&str>, Vec<&str>) =
        failed(&r.encode_checks, &r.encoders, "vaapi", Fault::Driver)
            .into_iter()
            .partition(|f| *f == "AV1");
    if !dec.is_empty() || !enc.is_empty() {
        let mut what = format!("VA-API on {node} cannot {}", cannot(&dec, &enc));
        // Fedora's Mesa turns off H.264 and HEVC, all profiles, both ways.
        if fx.distro == Distro::Fedora
            && fx.vendor == Vendor::Amd
            && !fx.has("mesa-va-drivers-freeworld")
            && (dec.contains(&"H.264 High") || dec.contains(&"HEVC Main"))
        {
            what = format!("Fedora's Mesa VA-API driver has H.264 and HEVC turned off: {what}");
        }
        out.push(Issue {
            severity: Severity::Warning,
            what,
            impact: CPU_FALLBACK.into(),
            fix: fx.vaapi_codecs(node),
        });
    }
    if !av1_dec.is_empty() || !av1_enc.is_empty() {
        out.push(Issue {
            severity: Severity::Info,
            what: format!("VA-API on {node} cannot {}", cannot(&av1_dec, &av1_enc)),
            impact: "AV1 decodes on the CPU (dav1d), and exports can't use the GPU for AV1".into(),
            fix: format!(
                "nothing to install if this GPU has no AV1 engine (decode: AMD before RDNA 2, \
                 Intel before Xe; encode: AMD before RDNA 3, Intel before Arc)\n{}",
                vainfo(node)
            ),
        });
    }

    // What its driver runs and gets wrong: a bug, not a missing feature.
    // The driver gives no error for it, so nothing falls back to the CPU,
    // which happens only when a device refuses (D3).
    let dec = failures(&r.decode_checks, &r.decoders, "vaapi", Fault::WrongOutput);
    let enc = failures(&r.encode_checks, &r.encoders, "vaapi", Fault::WrongOutput);
    const SHOWN: &str = "the viewer and exports would show its wrong pictures";
    const ENCODED: &str = "exports it encodes would be wrong";
    let (what, used) = match (dec.is_empty(), enc.is_empty()) {
        (true, true) => return,
        (false, true) => (wrongly("decodes", &dec), SHOWN.to_string()),
        (true, false) => (wrongly("encodes", &enc), ENCODED.to_string()),
        (false, false) => (
            format!(
                "{}, and {}",
                wrongly("decodes", &dec),
                wrongly("encodes", &enc)
            ),
            format!("{SHOWN}, and {ENCODED}"),
        ),
    };
    out.push(Issue {
        severity: Severity::Warning,
        what: format!("VA-API on {node} {what}"),
        impact: format!("the driver gives no error, so nothing falls back to the CPU: {used}"),
        fix: lines([
            Some(fx.newer_driver("the VA-API driver")),
            Some(format!(
                "vainfo --display drm --device {node}   (names the driver and its version)"
            )),
        ]),
    });
}

const NO_VULKAN_GPU: &str = "the viewer and exports render with Vulkan, and on the CPU \
                             that is far too slow for 360 video; Vulkan Video decode is \
                             unavailable";

fn vulkan_issues(r: &DoctorReport, env: &Env, fx: &Fixes<'_>, out: &mut Vec<Issue>) {
    if !r.hw_device_types.iter().any(|t| t == "vulkan") {
        out.push(Issue {
            severity: Severity::Info,
            what: "this FFmpeg was built without Vulkan".into(),
            impact: "doctor cannot list the Vulkan devices, and FFmpeg's Vulkan Video decode \
                     is unavailable"
                .into(),
            fix: fx.ffmpeg(),
        });
        return;
    }
    let Some(dev) = r.devices.iter().find(|d| d.kind == "vulkan") else {
        return;
    };
    if !dev.ok {
        out.push(Issue {
            severity: Severity::Warning,
            what: format!(
                "FFmpeg cannot open a Vulkan device ({})",
                dev.error.as_deref().unwrap_or("?")
            ),
            impact: NO_VULKAN_GPU.into(),
            fix: fx.vulkan_driver(),
        });
        return;
    }
    if let Some(e) = &env.vulkan_error {
        out.push(Issue {
            severity: Severity::Info,
            what: format!("cannot list the Vulkan devices: {e}"),
            impact: "doctor cannot tell whether Vulkan runs on a GPU".into(),
            fix: "vulkaninfo --summary   (lists them)".into(),
        });
        return;
    }
    let cpus: Vec<&VulkanDevice> = r.vulkan_devices.iter().filter(|d| d.is_cpu()).collect();
    let gpu = r.vulkan_devices.iter().find(|d| !d.is_cpu());
    match gpu {
        None if !cpus.is_empty() => {
            let names: Vec<&str> = cpus.iter().map(|d| d.name.as_str()).collect();
            let what = match names[..] {
                [one] => format!("the only Vulkan device is {one}, which runs on the CPU"),
                _ => format!(
                    "the only Vulkan devices, {}, run on the CPU",
                    and_list(&names)
                ),
            };
            out.push(Issue {
                severity: Severity::Warning,
                what: format!("{what}: no GPU acceleration"),
                impact: NO_VULKAN_GPU.into(),
                fix: fx.vulkan_gpu(),
            });
        }
        Some(gpu) if cpus.iter().any(|d| d.used_by_ffmpeg) => {
            out.push(Issue {
                severity: Severity::Info,
                what: format!(
                    "FFmpeg's Vulkan device runs on the CPU, though {} is there",
                    gpu.name
                ),
                impact: "FFmpeg's Vulkan Video decode is unavailable".into(),
                fix: "MESA_VK_DEVICE_SELECT=list vulkaninfo   (lists the devices; set \
                      MESA_VK_DEVICE_SELECT=<vendor:device> to pick the GPU)"
                    .into(),
            });
        }
        _ => vulkan_decode_issues(r, fx, out),
    }
}

/// What Vulkan Video can't decode, or decodes wrongly, on FFmpeg's Vulkan
/// device, a GPU. Only notes: VA-API is the hardware decoder, and Vulkan
/// Video the second one.
fn vulkan_decode_issues(r: &DoctorReport, fx: &Fixes<'_>, out: &mut Vec<Issue>) {
    // What decodes `formats`, which Vulkan Video refuses or, with
    // `vulkan_wrong`, decodes wrongly. VA-API comes first. Only a refusal
    // falls back, to Vulkan Video and then the CPU (D3): a driver that
    // decodes wrongly gives no error, so its pictures are used.
    let impact = |formats: &[&str], vulkan_wrong: bool| -> String {
        let (mut works, mut wrong, mut unable) = (Vec::new(), Vec::new(), Vec::new());
        for &f in formats {
            match r
                .decode_checks
                .iter()
                .find(|c| c.path == "vaapi" && c.format == f)
            {
                Some(c) if c.result == CheckResult::Works => works.push(f),
                Some(c) if c.fault == Some(Fault::WrongOutput) => wrong.push(f),
                _ => unable.push(f),
            }
        }
        if works.len() == formats.len() {
            return "none while VA-API decodes them, as it does here".into();
        }
        if unable.len() == formats.len() && !vulkan_wrong {
            return "with VA-API unable too, they decode on the CPU, which is far slower at \
                    5.7K and up"
                .into();
        }
        let mut parts = Vec::new();
        if !works.is_empty() {
            parts.push(format!("VA-API still decodes {}", and_list(&works)));
        }
        if !wrong.is_empty() {
            parts.push(format!("VA-API decodes {}, but wrongly", and_list(&wrong)));
        }
        if !unable.is_empty() && vulkan_wrong {
            parts.push(format!(
                "VA-API can't decode {}, and Vulkan Video gives no error, so nothing falls back \
                 to the CPU: the viewer and exports would show its wrong pictures",
                and_list(&unable)
            ));
        } else if !unable.is_empty() {
            parts.push(format!(
                "the CPU decodes {}, which is far slower at 5.7K and up",
                and_list(&unable)
            ));
        }
        parts.join("; ")
    };
    let ffmpeg_dec = failed(&r.decode_checks, &r.decoders, "vulkan", Fault::Ffmpeg);
    if !ffmpeg_dec.is_empty() {
        out.push(Issue {
            severity: Severity::Info,
            what: format!(
                "this FFmpeg cannot use Vulkan Video to decode {}",
                and_list(&ffmpeg_dec)
            ),
            impact: impact(&ffmpeg_dec, false),
            fix: fx.ffmpeg(),
        });
    }
    let dev = r.vulkan_devices.iter().find(|d| d.used_by_ffmpeg);
    let on = dev.map_or_else(|| "the Vulkan device".to_string(), |d| d.name.clone());
    let refused = failures(&r.decode_checks, &r.decoders, "vulkan", Fault::Driver);
    if let [first, ..] = refused[..] {
        // FFmpeg's first complaint says why, e.g. a missing video
        // extension; shown when every format failed with the same one.
        fn first_log(c: &Check) -> Option<&str> {
            c.ffmpeg_log.first().map(String::as_str)
        }
        let why = match first_log(first) {
            Some(line) if refused.iter().all(|c| first_log(c) == Some(line)) => {
                format!(" (FFmpeg: {})", line.trim())
            }
            _ => String::new(),
        };
        let formats = formats(&refused);
        out.push(Issue {
            severity: Severity::Info,
            what: format!(
                "Vulkan Video on {on} cannot decode {}{why}",
                and_list(&formats)
            ),
            impact: impact(&formats, false),
            fix: "vulkaninfo | grep -i video_decode   (the Vulkan Video formats this driver \
                  decodes; newer Mesa releases add more)"
                .into(),
        });
    }
    // A driver that decodes a format wrongly still lists it in vulkaninfo,
    // so the fix is a newer driver.
    let wrong = failures(&r.decode_checks, &r.decoders, "vulkan", Fault::WrongOutput);
    if !wrong.is_empty() {
        let driver = match dev.map(|d| (d.driver.as_deref(), d.driver_info.as_deref())) {
            Some((Some(name), Some(info))) => format!("{name} ({info})"),
            Some((Some(one), None) | (None, Some(one))) => one.to_string(),
            _ => "the Vulkan driver".to_string(),
        };
        out.push(Issue {
            severity: Severity::Info,
            what: format!("Vulkan Video on {on} {}", wrongly("decodes", &wrong)),
            impact: impact(&formats(&wrong), true),
            fix: fx.newer_driver(&driver),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn codecs(needs: &[Need]) -> Vec<CodecStatus> {
        needs
            .iter()
            .map(|n| CodecStatus {
                name: n.codec.to_string(),
                available: true,
                hw: if ["h264", "hevc", "av1"].contains(&n.codec) {
                    vec!["vaapi".into(), "vulkan".into()]
                } else {
                    vec![]
                },
            })
            .collect()
    }

    fn device(kind: &str, ok: bool) -> DeviceStatus {
        DeviceStatus {
            kind: kind.into(),
            device: (kind == "vaapi").then(|| "/dev/dri/renderD128".into()),
            ok,
            error: (!ok).then(|| "Invalid argument".into()),
            name: None,
            ffmpeg_log: vec![],
        }
    }

    fn gpu_device(name: &str, device_type: VulkanDeviceType) -> VulkanDevice {
        VulkanDevice {
            name: name.into(),
            device_type,
            vendor_id: "0x1002".into(),
            device_id: "0x7480".into(),
            api_version: "1.4.318".into(),
            driver: Some("radv".into()),
            driver_info: None,
            used_by_ffmpeg: true,
        }
    }

    /// A healthy AMD machine on `distro`: every check works.
    fn healthy(distro: Distro) -> DoctorReport {
        let mut decode_checks = Vec::new();
        for s in SAMPLES {
            for path in [SOFTWARE, "vaapi", "vulkan"] {
                decode_checks.push(Check::new(s.format, path, s.hardware, CheckResult::Works));
            }
        }
        DoctorReport {
            distro,
            ffmpeg_version: "n8.1".into(),
            libraries: BTreeMap::new(),
            hw_device_types: vec!["vaapi".into(), "vulkan".into()],
            decoders: codecs(DECODERS),
            encoders: codecs(ENCODERS),
            gpu: Gpu {
                render_node: "/dev/dri/renderD128".into(),
                vendor: Vendor::Amd,
                vendor_id: Some("0x1002".into()),
            },
            devices: vec![device("vaapi", true), device("vulkan", true)],
            vulkan_devices: vec![gpu_device(
                "AMD Radeon RX 7600 (RADV NAVI33)",
                VulkanDeviceType::DiscreteGpu,
            )],
            decode_checks,
            encode_checks: ENCODE_TARGETS
                .iter()
                .map(|t| Check::new(t.format, "vaapi", t.encoder, CheckResult::Works))
                .collect(),
            issues: vec![],
        }
    }

    fn fail(checks: &mut [Check], path: &str, formats: &[&str], fault: Fault) {
        for c in checks
            .iter_mut()
            .filter(|c| c.path == path && formats.contains(&c.format.as_str()))
        {
            c.result = CheckResult::Fails;
            c.fault = Some(fault);
            c.reason = Some("the vaapi driver cannot decode this format".into());
        }
    }

    fn installed(pkgs: &[&str]) -> Env {
        Env {
            installed: pkgs.iter().map(|p| p.to_string()).collect(),
            ..Env::default()
        }
    }

    fn only(issues: &[Issue]) -> &Issue {
        assert_eq!(issues.len(), 1, "{issues:#?}");
        &issues[0]
    }

    #[test]
    fn a_healthy_machine_has_no_issues() {
        for d in [Distro::Fedora, Distro::Arch, Distro::Other] {
            assert!(issues(&healthy(d), &Env::default()).is_empty());
        }
    }

    /// Fedora's stock Mesa: the device opens, H.264 and HEVC don't decode
    /// or encode, AV1 does.
    fn fedora_stock_mesa() -> DoctorReport {
        let mut r = healthy(Distro::Fedora);
        let codecs = ["H.264 High", "HEVC Main", "HEVC Main 10"];
        fail(&mut r.decode_checks, "vaapi", &codecs, Fault::Driver);
        let codecs = ["H.264", "HEVC Main", "HEVC Main 10"];
        fail(&mut r.encode_checks, "vaapi", &codecs, Fault::Driver);
        r
    }

    #[test]
    fn fedora_amd_with_stock_mesa_swaps_in_freeworld() {
        let r = fedora_stock_mesa();
        let env = installed(&["rpmfusion-free-release", "mesa-va-drivers"]);
        let i = only(&issues(&r, &env)).clone();
        assert_eq!(i.severity, Severity::Warning);
        assert_eq!(
            i.what,
            "Fedora's Mesa VA-API driver has H.264 and HEVC turned off: VA-API on \
             /dev/dri/renderD128 cannot decode H.264 High, HEVC Main and HEVC Main 10, \
             or encode H.264, HEVC Main and HEVC Main 10"
        );
        assert_eq!(
            i.fix,
            "sudo dnf swap mesa-va-drivers mesa-va-drivers-freeworld   (RPM Fusion free)"
        );

        // No Mesa VA-API driver installed yet, and no RPM Fusion either.
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(
            i.fix,
            "sudo dnf install https://mirrors.rpmfusion.org/free/fedora/\
             rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm\n\
             sudo dnf install mesa-va-drivers-freeworld   (RPM Fusion free)"
        );
    }

    #[test]
    fn freeworld_installed_but_failing_is_not_called_fedoras_mesa() {
        let r = fedora_stock_mesa();
        let env = installed(&["rpmfusion-free-release", "mesa-va-drivers-freeworld"]);
        let i = only(&issues(&r, &env)).clone();
        assert!(
            i.what.starts_with("VA-API on /dev/dri/renderD128 cannot"),
            "{i:?}"
        );
        assert!(i.fix.starts_with("sudo dnf upgrade --refresh"), "{i:?}");
    }

    #[test]
    fn intel_gets_intel_media_driver() {
        let mut r = fedora_stock_mesa();
        r.gpu.vendor = Vendor::Intel;
        let env = installed(&["rpmfusion-nonfree-release", "libva-intel-media-driver"]);
        let i = only(&issues(&r, &env)).clone();
        assert!(i.what.starts_with("VA-API on"), "{i:?}");
        assert_eq!(
            i.fix,
            "sudo dnf swap libva-intel-media-driver intel-media-driver --allowerasing   \
             (RPM Fusion nonfree)"
        );

        r.distro = Distro::Arch;
        let i = only(&issues(&r, &Env::default())).clone();
        assert!(
            i.fix
                .starts_with("sudo pacman -S --needed intel-media-driver\n"),
            "{i:?}"
        );
        assert!(i.fix.contains("LIBVA_DRIVER_NAME=iHD"), "{i:?}");
    }

    #[test]
    fn missing_av1_alone_is_only_a_note() {
        let mut r = healthy(Distro::Arch);
        fail(&mut r.decode_checks, "vaapi", &[AV1], Fault::Driver);
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(i.severity, Severity::Info);
        assert_eq!(
            i.what,
            "VA-API on /dev/dri/renderD128 cannot decode AV1 Main"
        );
    }

    #[test]
    fn an_ffmpeg_without_the_vaapi_hwaccel_is_blamed_on_ffmpeg() {
        let mut r = healthy(Distro::Fedora);
        fail(&mut r.decode_checks, "vaapi", &["HEVC Main"], Fault::Ffmpeg);
        let env = installed(&["rpmfusion-free-release", "ffmpeg-free"]);
        let i = only(&issues(&r, &env)).clone();
        assert_eq!(i.what, "this FFmpeg cannot use VA-API to decode HEVC Main");
        assert_eq!(
            i.fix,
            "sudo dnf swap ffmpeg-free ffmpeg --allowerasing   (RPM Fusion's full FFmpeg)"
        );
    }

    #[test]
    fn an_ffmpeg_without_vaapi_gets_the_ffmpeg_fix_not_a_driver() {
        let mut r = healthy(Distro::Arch);
        r.hw_device_types.retain(|t| t != "vaapi");
        r.devices[0] = DeviceStatus {
            error: Some("this FFmpeg was built without vaapi support".into()),
            ..device("vaapi", false)
        };
        for c in r.decoders.iter_mut() {
            c.hw.retain(|h| h != "vaapi");
        }
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(i.what, "this FFmpeg was built without VA-API");
        assert_eq!(i.fix, "sudo pacman -S --needed ffmpeg");
    }

    #[test]
    fn missing_decoders_are_errors_with_the_ffmpeg_fix() {
        let mut r = healthy(Distro::Arch);
        for name in ["libdav1d", "prores", "aac"] {
            r.decoders
                .iter_mut()
                .find(|c| c.name == name)
                .unwrap()
                .available = false;
        }
        fail(&mut r.decode_checks, SOFTWARE, &[AV1], Fault::Ffmpeg);
        // AV1 still decodes on the GPU, so its missing software decoder is
        // a warning.
        let is = issues(&r, &Env::default());
        let whats: Vec<_> = is.iter().map(|i| (i.severity, i.what.as_str())).collect();
        assert_eq!(
            whats,
            [
                (Severity::Error, "FFmpeg has no ProRes decoder (prores)"),
                (Severity::Error, "FFmpeg has no AAC decoder (aac)"),
                (
                    Severity::Warning,
                    "FFmpeg has no working software AV1 decoder (libdav1d)"
                ),
            ]
        );
        assert!(is.iter().all(|i| i.fix == "sudo pacman -S --needed ffmpeg"));

        fail(&mut r.decode_checks, "vaapi", &[AV1], Fault::Driver);
        fail(&mut r.decode_checks, "vulkan", &[AV1], Fault::Driver);
        let is = issues(&r, &Env::default());
        let av1 = is.iter().find(|i| i.what.contains("AV1 decoder")).unwrap();
        assert_eq!(av1.severity, Severity::Error);
        assert_eq!(av1.impact, "AV1 files cannot be opened");
    }

    /// `r` with `codec` gone from the decoders or the encoders.
    fn without(r: &DoctorReport, codec: &str, decoder: bool) -> DoctorReport {
        let mut r = r.clone();
        let list = if decoder {
            &mut r.decoders
        } else {
            &mut r.encoders
        };
        list.iter_mut().find(|c| c.name == codec).unwrap().available = false;
        r
    }

    #[test]
    fn every_missing_codec_gets_an_issue_with_the_ffmpeg_fix() {
        // The codec tables show each one as missing; something must say
        // what that costs and how to fix it.
        let base = healthy(Distro::Arch);
        let mut all = base.clone();
        let listed = DECODERS
            .iter()
            .map(|n| (n, true))
            .chain(ENCODERS.iter().map(|n| (n, false)));
        for (n, decoder) in listed {
            let named = format!("({})", n.codec);
            let r = without(&base, n.codec, decoder);
            let i = only(&issues(&r, &Env::default())).clone();
            assert!(i.what.contains(&named), "{}: {i:?}", n.codec);
            assert_eq!(i.severity, n.severity, "{}", n.codec);
            assert_eq!(i.impact, n.impact, "{}", n.codec);
            assert_eq!(i.fix, "sudo pacman -S --needed ffmpeg", "{}", n.codec);
            all = without(&all, n.codec, decoder);
        }
        // All at once, each one is still named.
        let is = issues(&all, &Env::default());
        for n in DECODERS.iter().chain(ENCODERS) {
            let named = format!("({})", n.codec);
            assert!(
                is.iter().any(|i| i.what.contains(&named)),
                "{named}: {is:#?}"
            );
        }

        let i = only(&issues(
            &without(&base, "libsvtav1", false),
            &Env::default(),
        ))
        .clone();
        assert_eq!(i.what, "FFmpeg has no software AV1 encoder (libsvtav1)");
        assert_eq!(
            i.impact,
            "AV1 export needs VA-API; there is no CPU fallback"
        );
        let i = only(&issues(&without(&base, "aac", false), &Env::default())).clone();
        assert_eq!(i.what, "FFmpeg has no AAC encoder (aac)");

        // On other distros the fix names what a full build has.
        let mut r = without(&base, "libsvtav1", false);
        r.distro = Distro::Other;
        assert_eq!(
            only(&issues(&r, &Env::default())).fix,
            "install a full FFmpeg build: H.264/HEVC, libdav1d, libx264/libx265, SVT-AV1, \
             VA-API and Vulkan"
        );
    }

    #[test]
    fn a_missing_codec_is_not_also_blamed_on_vaapi_or_vulkan() {
        // As doctor finds it: the hardware checks of that format fail for
        // lack of the decoder or encoder.
        let mut r = without(&healthy(Distro::Arch), "av1", true);
        r = without(&r, "h264_vaapi", false);
        for path in ["vaapi", "vulkan"] {
            fail(&mut r.decode_checks, path, &[AV1], Fault::Ffmpeg);
        }
        fail(&mut r.encode_checks, "vaapi", &["H.264"], Fault::Ffmpeg);
        let is = issues(&r, &Env::default());
        let whats: Vec<_> = is.iter().map(|i| i.what.as_str()).collect();
        assert_eq!(
            whats,
            [
                "FFmpeg has no VA-API H.264 encoder (h264_vaapi)",
                "FFmpeg has no AV1 decoder for VA-API and Vulkan Video (av1)",
            ]
        );

        // Without VA-API at all, that one issue covers its encoders.
        let mut r = healthy(Distro::Arch);
        r.hw_device_types.retain(|t| t != "vaapi");
        r.devices[0] = device("vaapi", false);
        for c in r.decoders.iter_mut() {
            c.hw.retain(|h| h != "vaapi");
        }
        for e in ["h264_vaapi", "hevc_vaapi", "av1_vaapi"] {
            r = without(&r, e, false);
        }
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(i.what, "this FFmpeg was built without VA-API");
    }

    #[test]
    fn cpu_only_vulkan_is_flagged_with_the_driver_fix() {
        let mut r = healthy(Distro::Arch);
        r.vulkan_devices = vec![VulkanDevice {
            driver: Some("llvmpipe".into()),
            ..gpu_device("llvmpipe (LLVM 20.1.2, 256 bits)", VulkanDeviceType::Cpu)
        }];
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(i.severity, Severity::Warning);
        assert_eq!(
            i.what,
            "the only Vulkan device is llvmpipe (LLVM 20.1.2, 256 bits), which runs on the \
             CPU: no GPU acceleration"
        );
        assert_eq!(
            i.fix,
            "sudo pacman -S --needed vulkan-radeon vulkan-icd-loader"
        );

        // No render node and no GPU vendor known: both Arch drivers, after
        // the pass-through hint.
        r.gpu.vendor = Vendor::Unknown;
        let env = Env {
            node_error: Some((ErrorKind::NotFound, "not found".into())),
            ..Env::default()
        };
        let i = issues(&r, &env)
            .into_iter()
            .find(|i| i.what.contains("Vulkan"))
            .unwrap();
        assert_eq!(
            i.fix,
            "no GPU is visible here: in a VM or container, pass it through first (for \
             example `--device /dev/dri`)\n\
             # AMD GPU:\nsudo pacman -S --needed vulkan-radeon vulkan-icd-loader\n\
             # Intel GPU:\nsudo pacman -S --needed vulkan-intel vulkan-icd-loader"
        );

        // A GPU next to llvmpipe is fine.
        r.vulkan_devices.push(gpu_device(
            "AMD Radeon 8060S",
            VulkanDeviceType::IntegratedGpu,
        ));
        r.vulkan_devices[1].used_by_ffmpeg = false;
        r.vulkan_devices[0].used_by_ffmpeg = false;
        assert!(issues(&r, &Env::default()).is_empty());
    }

    #[test]
    fn cpu_only_vulkan_on_fedora_with_mesa_installed_asks_why_it_does_not_load() {
        let mut r = healthy(Distro::Fedora);
        let llvmpipe = gpu_device("llvmpipe (LLVM 20.1.2, 256 bits)", VulkanDeviceType::Cpu);
        r.vulkan_devices = vec![llvmpipe.clone()];
        let env = installed(&["mesa-vulkan-drivers"]);
        let i = only(&issues(&r, &env)).clone();
        assert!(
            i.fix
                .starts_with("VK_LOADER_DEBUG=error,warn vulkaninfo --summary"),
            "{i:?}"
        );
        // In a VM or container with no GPU passed through, that is the
        // whole fix: the drivers are already installed.
        let no_node = |pkgs: &[&str]| Env {
            node_error: Some((ErrorKind::NotFound, "not found".into())),
            ..installed(pkgs)
        };
        let vulkan_fix = |env: &Env| {
            issues(&r, env)
                .into_iter()
                .find(|i| i.what.contains("Vulkan"))
                .unwrap()
                .fix
        };
        assert_eq!(
            vulkan_fix(&no_node(&["mesa-vulkan-drivers"])),
            "no GPU is visible here: in a VM or container, pass it through first (for \
             example `--device /dev/dri`)"
        );
        // Not installed: install it, after passing the GPU through.
        assert_eq!(
            vulkan_fix(&no_node(&[])),
            "no GPU is visible here: in a VM or container, pass it through first (for \
             example `--device /dev/dri`)\n\
             sudo dnf install mesa-vulkan-drivers vulkan-loader   (RADV for AMD, ANV for Intel)"
        );
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(
            i.fix,
            "sudo dnf install mesa-vulkan-drivers vulkan-loader   (RADV for AMD, ANV for Intel)"
        );

        // Two CPU devices read as a plural.
        r.vulkan_devices
            .push(gpu_device("SwiftShader", VulkanDeviceType::Cpu));
        let i = only(&issues(&r, &env)).clone();
        assert_eq!(
            i.what,
            "the only Vulkan devices, llvmpipe (LLVM 20.1.2, 256 bits) and SwiftShader, run \
             on the CPU: no GPU acceleration"
        );
    }

    #[test]
    fn cpu_only_vulkan_without_render_node_permission_adds_the_user_to_render() {
        let mut r = healthy(Distro::Arch);
        r.devices[0] = device("vaapi", false);
        r.vulkan_devices = vec![gpu_device("llvmpipe", VulkanDeviceType::Cpu)];
        let env = Env {
            node_error: Some((ErrorKind::PermissionDenied, String::new())),
            ..Env::default()
        };
        let is = issues(&r, &env);
        assert_eq!(is.len(), 2, "{is:#?}");
        assert!(
            is.iter()
                .all(|i| i.fix.starts_with("sudo usermod -aG render"))
        );
    }

    #[test]
    fn vulkan_video_failures_on_a_gpu_are_notes_with_ffmpegs_reason() {
        let mut r = healthy(Distro::Fedora);
        let hevc = ["HEVC Main", "HEVC Main 10"];
        fail(&mut r.decode_checks, "vulkan", &hevc, Fault::Driver);
        for c in r.decode_checks.iter_mut().filter(|c| c.path == "vulkan") {
            c.ffmpeg_log =
                vec!["Device does not support the VK_KHR_video_decode_h265 extension!\n".into()];
        }
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(i.severity, Severity::Info);
        assert_eq!(
            i.what,
            "Vulkan Video on AMD Radeon RX 7600 (RADV NAVI33) cannot decode HEVC Main and \
             HEVC Main 10 (FFmpeg: Device does not support the VK_KHR_video_decode_h265 \
             extension!)"
        );
        assert_eq!(i.impact, "none while VA-API decodes them, as it does here");

        assert_eq!(
            i.fix,
            "vulkaninfo | grep -i video_decode   (the Vulkan Video formats this driver \
             decodes; newer Mesa releases add more)"
        );

        // VA-API can't decode HEVC Main either: the CPU does, and only that.
        let vulkan_note = |r: &DoctorReport| {
            issues(r, &Env::default())
                .into_iter()
                .find(|i| i.what.starts_with("Vulkan Video"))
                .unwrap()
        };
        fail(&mut r.decode_checks, "vaapi", &["HEVC Main"], Fault::Driver);
        assert_eq!(
            vulkan_note(&r).impact,
            "VA-API still decodes HEVC Main 10; the CPU decodes HEVC Main, which is far \
             slower at 5.7K and up"
        );
        fail(
            &mut r.decode_checks,
            "vaapi",
            &["HEVC Main 10"],
            Fault::Driver,
        );
        assert_eq!(
            vulkan_note(&r).impact,
            "with VA-API unable too, they decode on the CPU, which is far slower at 5.7K and up"
        );

        // A build without the Vulkan hwaccel is FFmpeg's fault.
        let mut r = healthy(Distro::Arch);
        fail(&mut r.decode_checks, "vulkan", &[AV1], Fault::Ffmpeg);
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(
            i.what,
            "this FFmpeg cannot use Vulkan Video to decode AV1 Main"
        );
        assert_eq!(i.fix, "sudo pacman -S --needed ffmpeg");
    }

    fn wrong(checks: &mut [Check], path: &str, format: &str, reason: &str) {
        fail(checks, path, &[format], Fault::WrongOutput);
        for c in checks
            .iter_mut()
            .filter(|c| c.path == path && c.format == format)
        {
            c.reason = Some(reason.into());
        }
    }

    #[test]
    fn a_driver_that_decodes_wrongly_is_told_apart_from_one_that_refuses() {
        // A wrong picture is the driver's bug, not a missing feature.
        let e = compare(
            &[vec![1, 2], vec![3], vec![4]],
            &[vec![1, 0], vec![3], vec![4]],
        )
        .unwrap_err();
        assert_eq!(e.fault, Fault::WrongOutput);
        assert_eq!(
            e.reason,
            "1 of 4 samples differ from FFmpeg's software decoder"
        );

        let mut r = healthy(Distro::Fedora);
        r.vulkan_devices[0].driver_info = Some("Mesa 25.2.8".into());
        fail(
            &mut r.decode_checks,
            "vulkan",
            &["H.264 High"],
            Fault::Driver,
        );
        let differ = "12 of 98304 samples differ from FFmpeg's software decoder";
        wrong(&mut r.decode_checks, "vulkan", "HEVC Main", differ);
        let is = issues(&r, &Env::default());
        assert_eq!(is.len(), 2, "{is:#?}");
        assert_eq!(
            is[0].what,
            "Vulkan Video on AMD Radeon RX 7600 (RADV NAVI33) cannot decode H.264 High"
        );
        assert!(is[0].fix.starts_with("vulkaninfo"), "{:?}", is[0]);
        assert_eq!(is[1].severity, Severity::Info);
        assert_eq!(
            is[1].what,
            "Vulkan Video on AMD Radeon RX 7600 (RADV NAVI33) decodes HEVC Main wrongly (12 \
             of 98304 samples differ from FFmpeg's software decoder)"
        );
        assert_eq!(
            is[1].impact,
            "none while VA-API decodes them, as it does here"
        );
        assert_eq!(
            is[1].fix,
            "sudo dnf upgrade --refresh   (radv (Mesa 25.2.8) gets this wrong: a newer \
             release may fix it; if not, report it to its developers)"
        );

        // Several formats each give their reason.
        wrong(
            &mut r.decode_checks,
            "vulkan",
            "HEVC Main 10",
            "decodes to 128x128 instead of 256x256",
        );
        let i = issues(&r, &Env::default()).pop().unwrap();
        assert_eq!(
            i.what,
            "Vulkan Video on AMD Radeon RX 7600 (RADV NAVI33) decodes HEVC Main and HEVC \
             Main 10 wrongly (HEVC Main: 12 of 98304 samples differ from FFmpeg's software \
             decoder; HEVC Main 10: decodes to 128x128 instead of 256x256)"
        );

        // On VA-API, the main hardware decoder, it is a warning.
        let mut r = healthy(Distro::Arch);
        wrong(&mut r.decode_checks, "vaapi", "HEVC Main", differ);
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(i.severity, Severity::Warning);
        assert_eq!(
            i.what,
            "VA-API on /dev/dri/renderD128 decodes HEVC Main wrongly (12 of 98304 samples \
             differ from FFmpeg's software decoder)"
        );
        // Wrong output comes without an error, so nothing falls back.
        assert_eq!(
            i.impact,
            "the driver gives no error, so nothing falls back to the CPU: the viewer and \
             exports would show its wrong pictures"
        );
        assert_eq!(
            i.fix,
            "sudo pacman -Syu   (the VA-API driver gets this wrong: a newer release may fix \
             it; if not, report it to its developers)\n\
             vainfo --display drm --device /dev/dri/renderD128   (names the driver and its \
             version)"
        );
        let unreadable = "its output doesn't decode: Invalid data found when processing input";
        wrong(&mut r.encode_checks, "vaapi", "H.264", unreadable);
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(
            i.what,
            "VA-API on /dev/dri/renderD128 decodes HEVC Main wrongly (12 of 98304 samples \
             differ from FFmpeg's software decoder), and encodes H.264 wrongly (its output \
             doesn't decode: Invalid data found when processing input)"
        );
        assert_eq!(
            i.impact,
            "the driver gives no error, so nothing falls back to the CPU: the viewer and \
             exports would show its wrong pictures, and exports it encodes would be wrong"
        );
        let mut r = healthy(Distro::Arch);
        wrong(&mut r.encode_checks, "vaapi", "H.264", unreadable);
        assert_eq!(
            only(&issues(&r, &Env::default())).impact,
            "the driver gives no error, so nothing falls back to the CPU: exports it encodes \
             would be wrong"
        );
    }

    #[test]
    fn vulkan_notes_say_what_decodes_when_one_driver_refuses_and_the_other_is_wrong() {
        let differ = "12 of 98304 samples differ from FFmpeg's software decoder";
        let vulkan_note = |r: &DoctorReport| {
            issues(r, &Env::default())
                .into_iter()
                .find(|i| i.what.starts_with("Vulkan Video"))
                .unwrap()
        };
        let hevc = ["HEVC Main", "HEVC Main 10"];

        // Vulkan Video decodes wrongly where VA-API refuses: its pictures
        // are used, since only a refusal falls back.
        let mut r = healthy(Distro::Arch);
        for f in hevc {
            wrong(&mut r.decode_checks, "vulkan", f, differ);
        }
        fail(&mut r.decode_checks, "vaapi", &["HEVC Main"], Fault::Driver);
        assert_eq!(
            vulkan_note(&r).impact,
            "VA-API still decodes HEVC Main 10; VA-API can't decode HEVC Main, and Vulkan \
             Video gives no error, so nothing falls back to the CPU: the viewer and exports \
             would show its wrong pictures"
        );
        fail(
            &mut r.decode_checks,
            "vaapi",
            &["HEVC Main 10"],
            Fault::Driver,
        );
        assert_eq!(
            vulkan_note(&r).impact,
            "VA-API can't decode HEVC Main and HEVC Main 10, and Vulkan Video gives no \
             error, so nothing falls back to the CPU: the viewer and exports would show its \
             wrong pictures"
        );
        // Wrong on both: VA-API comes first, and its pictures are used.
        wrong(&mut r.decode_checks, "vaapi", "HEVC Main 10", differ);
        assert_eq!(
            vulkan_note(&r).impact,
            "VA-API decodes HEVC Main 10, but wrongly; VA-API can't decode HEVC Main, and \
             Vulkan Video gives no error, so nothing falls back to the CPU: the viewer and \
             exports would show its wrong pictures"
        );

        // The reverse: Vulkan Video refuses what VA-API decodes wrongly.
        // Nothing falls back from VA-API, so the CPU doesn't decode it.
        let mut r = healthy(Distro::Arch);
        fail(&mut r.decode_checks, "vulkan", &hevc, Fault::Driver);
        wrong(&mut r.decode_checks, "vaapi", "HEVC Main", differ);
        assert_eq!(
            vulkan_note(&r).impact,
            "VA-API still decodes HEVC Main 10; VA-API decodes HEVC Main, but wrongly"
        );
        fail(
            &mut r.decode_checks,
            "vaapi",
            &["HEVC Main 10"],
            Fault::Driver,
        );
        assert_eq!(
            vulkan_note(&r).impact,
            "VA-API decodes HEVC Main, but wrongly; the CPU decodes HEVC Main 10, which is \
             far slower at 5.7K and up"
        );
    }

    #[test]
    fn fedora_installs_full_ffmpeg_allowing_erasure() {
        let mut r = healthy(Distro::Fedora);
        r.decoders
            .iter_mut()
            .find(|c| c.name == "aac")
            .unwrap()
            .available = false;
        let i = only(&issues(&r, &installed(&["rpmfusion-free-release"]))).clone();
        assert_eq!(
            i.fix,
            "sudo dnf install ffmpeg --allowerasing   (RPM Fusion's full FFmpeg)"
        );
    }

    #[test]
    fn a_missing_vulkan_device_gets_the_driver_fix() {
        let mut r = healthy(Distro::Fedora);
        r.devices[1] = device("vulkan", false);
        r.vulkan_devices.clear();
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(
            i.what,
            "FFmpeg cannot open a Vulkan device (Invalid argument)"
        );
        assert_eq!(
            i.fix,
            "sudo dnf install mesa-vulkan-drivers vulkan-loader   (RADV for AMD, ANV for Intel)"
        );
    }

    #[test]
    fn no_render_node_and_no_permission_are_told_apart() {
        let mut r = healthy(Distro::Fedora);
        r.devices[0] = device("vaapi", false);
        for (kind, what, fix) in [
            (
                ErrorKind::NotFound,
                "no DRM render node at /dev/dri/renderD128",
                "pass the GPU through",
            ),
            (
                ErrorKind::PermissionDenied,
                "no permission to open /dev/dri/renderD128",
                "usermod -aG render",
            ),
        ] {
            let env = Env {
                node_error: Some((kind, String::new())),
                ..Env::default()
            };
            let i = only(&issues(&r, &env)).clone();
            assert_eq!(i.what, what);
            assert!(i.fix.contains(fix), "{i:?}");
        }
        // The node opens, VA-API doesn't: the driver for this GPU.
        let i = only(&issues(&r, &Env::default())).clone();
        assert_eq!(
            i.what,
            "no VA-API driver for /dev/dri/renderD128 (Invalid argument)"
        );
        assert!(
            i.fix
                .ends_with("sudo dnf install mesa-va-drivers-freeworld   (RPM Fusion free)")
        );
    }

    #[test]
    fn lists_read_naturally() {
        assert_eq!(and_list(&["a"]), "a");
        assert_eq!(and_list(&["a", "b"]), "a and b");
        assert_eq!(and_list(&["a", "b", "c"]), "a, b and c");
        assert_eq!(cannot(&["X"], &[]), "decode X");
        assert_eq!(cannot(&[], &["Y", "Z"]), "encode Y and Z");
    }
}
