//! Human-readable text for CLI output.

use std::fmt::Write as _;

use spherecut_core::Rational;
use spherecut_media::doctor::{
    Check, CheckResult, DoctorReport, SAMPLE_SIZE, Severity, VulkanDeviceType,
};
use spherecut_media::probe::{ProbeReport, StreamInfo, StructureCheck, TruncatedBox};
use spherecut_media::spherical::{Projection, StereoMode};

fn secs(r: Rational) -> String {
    format!("{:.3} s", r.to_f64())
}

fn rate(r: Rational) -> String {
    if r.den() == 1 {
        format!("{} fps", r.num())
    } else {
        format!("{} ({:.3}) fps", r, r.to_f64())
    }
}

fn opt<T: std::fmt::Display>(o: &Option<T>) -> String {
    o.as_ref().map_or_else(|| "-".into(), |v| v.to_string())
}

fn truncated(t: &TruncatedBox) -> String {
    let fourcc = t.fourcc.as_ref().map(|f| format!(" `{f}`"));
    match (fourcc, t.size) {
        (Some(f), Some(size)) if t.present >= t.header_len => {
            format!(", truncated{f} ({} of {size} bytes)", t.present)
        }
        (f, _) => format!(
            ", truncated{} box header ({} of {} bytes)",
            f.unwrap_or_default(),
            t.present,
            t.header_len
        ),
    }
}

pub fn probe(r: &ProbeReport) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{}", r.path.display());
    let _ = writeln!(
        s,
        "  container  {} ({}), {} bytes{}{}",
        r.container.format,
        r.container.format_long,
        r.file_size,
        r.container
            .duration
            .map(|d| format!(", {}", secs(d)))
            .unwrap_or_default(),
        r.container
            .bit_rate
            .map(|b| format!(", {:.1} Mb/s", b as f64 / 1e6))
            .unwrap_or_default()
    );
    if let Some(m) = &r.mp4 {
        let boxes: Vec<String> = m.top_level.iter().map(|(t, _)| t.clone()).collect();
        let _ = writeln!(
            s,
            "  mp4        brand {}, boxes [{}]{}{}",
            opt(&m.brand),
            boxes.join(" "),
            m.truncated.as_ref().map(truncated).unwrap_or_default(),
            if m.trailing_bytes > 0 {
                format!(", {} trailing bytes", m.trailing_bytes)
            } else {
                String::new()
            }
        );
    }
    match &r.structure {
        StructureCheck::Full => {}
        StructureCheck::Length => {
            let _ = writeln!(
                s,
                "  structure  only its length is checked; damage inside goes unnoticed"
            );
        }
        StructureCheck::None { why } => {
            let _ = writeln!(
                s,
                "  structure  not checked ({why}); truncation and corruption go unnoticed"
            );
        }
    }
    if let Some(c) = &r.camera {
        let _ = writeln!(
            s,
            "  camera     {}{}{}  layout: {:?}",
            opt(&c.vendor),
            c.model
                .as_ref()
                .map(|m| format!(" {m}"))
                .unwrap_or_default(),
            c.firmware
                .as_ref()
                .map(|f| format!(" (firmware {f})"))
                .unwrap_or_default(),
            c.layout
        );
        for e in &c.evidence {
            let _ = writeln!(s, "             - {e}");
        }
    }
    for st in &r.streams {
        stream(&mut s, st);
    }
    for (k, v) in &r.container.tags {
        let _ = writeln!(s, "  tag        {k} = {v}");
    }
    for w in &r.warnings {
        let _ = writeln!(s, "  warning    {w}");
    }
    s += &ffmpeg_lines(&r.ffmpeg_log);
    for d in &r.damage {
        let _ = writeln!(s, "  DAMAGE     {d}");
    }
    s
}

/// The lines of FFmpeg's log about a file, as probe's report shows them.
pub fn ffmpeg_lines(log: &[String]) -> String {
    log.iter().map(|l| format!("  ffmpeg     {l}\n")).collect()
}

fn stream(s: &mut String, st: &StreamInfo) {
    let _ = write!(
        s,
        "  #{:<2} {:<7} {}",
        st.index,
        format!("{:?}", st.kind).to_lowercase(),
        opt(&st.codec)
    );
    if let Some(t) = &st.codec_tag {
        let _ = write!(s, " [{t}]");
    }
    if let Some(p) = &st.profile {
        let _ = write!(s, " {p}");
    }
    if let Some(v) = &st.video {
        let _ = write!(
            s,
            ", {}x{}, {}{}",
            v.width,
            v.height,
            opt(&v.pix_fmt),
            v.bit_depth
                .map(|b| format!(" ({b}-bit)"))
                .unwrap_or_default()
        );
    }
    if let Some(a) = &st.audio {
        let _ = write!(
            s,
            ", {} Hz, {} ch{}",
            a.sample_rate,
            a.channels,
            a.layout
                .as_ref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default()
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "             timebase {}, {}{}{}",
        st.time_base,
        st.avg_frame_rate
            .map(rate)
            .unwrap_or_else(|| "no frame rate".into()),
        st.duration
            .map(|d| format!(", {}", secs(d)))
            .unwrap_or_default(),
        st.frames
            .map(|f| format!(", {f} frames"))
            .unwrap_or_default()
    );
    if let Some(v) = &st.video {
        let c = &v.color;
        let _ = writeln!(
            s,
            "             color range {}, matrix {}, primaries {}, transfer {}",
            opt(&c.range),
            opt(&c.matrix),
            opt(&c.primaries),
            opt(&c.transfer)
        );
        if let Some(rot) = v.rotation_deg.filter(|r| r.abs() > 1e-6) {
            let _ = writeln!(s, "             display rotation {rot}°");
        }
    }
    if let Some(h) = &st.handler {
        let _ = writeln!(s, "             handler \"{h}\"");
    }
    if !st.disposition.is_empty() {
        let _ = writeln!(s, "             disposition {}", st.disposition.join(", "));
    }
    if let Some(v2) = &st.spherical.v2 {
        let proj = match &v2.projection {
            Some(Projection::Equirect {
                top,
                bottom,
                left,
                right,
            }) => {
                if [top, bottom, left, right].iter().all(|v| **v == 0) {
                    "equirect (full sphere)".to_string()
                } else {
                    format!("equirect (cropped t{top} b{bottom} l{left} r{right})")
                }
            }
            Some(Projection::Cubemap { layout, padding }) => {
                format!("cubemap (layout {layout}, padding {padding})")
            }
            Some(Projection::Mesh) => "mesh".into(),
            Some(Projection::Other { fourcc }) => format!("unknown `{fourcc}`"),
            None => "no projection box".into(),
        };
        let stereo = match v2.stereo {
            Some(StereoMode::Mono) => "mono".to_string(),
            Some(StereoMode::TopBottom) => "stereo top-bottom".into(),
            Some(StereoMode::LeftRight) => "stereo left-right".into(),
            Some(StereoMode::StereoCustom) => "stereo (custom)".into(),
            Some(StereoMode::Unknown(v)) => format!("unknown stereo mode {v}"),
            None => "no st3d (mono)".into(),
        };
        let _ = writeln!(
            s,
            "             spherical v2: {proj}, {stereo}, pose y{} p{} r{}{}",
            v2.pose_yaw,
            v2.pose_pitch,
            v2.pose_roll,
            v2.metadata_source
                .as_ref()
                .map(|m| format!(", source \"{m}\""))
                .unwrap_or_default()
        );
    }
    if let Some(v1) = &st.spherical.v1 {
        let _ = writeln!(
            s,
            "             spherical v1: spherical={} stitched={} projection {} software {}",
            v1.spherical,
            v1.stitched,
            opt(&v1.projection_type),
            opt(&v1.stitching_software)
        );
    }
    if let Some(sa) = &st.spherical.spatial_audio {
        let _ = writeln!(
            s,
            "             spatial audio: order {}, {}, channel map {:?}",
            sa.ambisonic_order,
            if sa.is_ambix() {
                "ambiX (ACN/SN3D)"
            } else {
                "non-ambiX ordering/normalization"
            },
            sa.channel_map
        );
    }
}

pub fn doctor(r: &DoctorReport) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "FFmpeg {}", r.ffmpeg_version);
    let libs: Vec<String> = r
        .libraries
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect();
    let _ = writeln!(s, "  {}", libs.join(", "));
    let _ = writeln!(
        s,
        "  hardware device types: {}",
        r.hw_device_types.join(", ")
    );
    let _ = writeln!(s, "\nDecoders");
    for d in &r.decoders {
        let _ = writeln!(
            s,
            "  {:<10} {}{}",
            d.name,
            if d.available { "yes" } else { "MISSING" },
            if d.hw.is_empty() {
                String::new()
            } else {
                format!("  (hw: {})", d.hw.join(", "))
            }
        );
    }
    let _ = writeln!(s, "\nEncoders");
    for e in &r.encoders {
        let _ = writeln!(
            s,
            "  {:<10} {}",
            e.name,
            if e.available { "yes" } else { "missing" }
        );
    }
    let _ = writeln!(s, "\nHardware devices");
    for d in &r.devices {
        let _ = writeln!(
            s,
            "  {:<7} {:<22} {}",
            d.kind,
            d.device.as_deref().unwrap_or("(default)"),
            match (d.ok, &d.name) {
                (true, Some(n)) => format!("ok, on {n}"),
                (true, None) => "ok".to_string(),
                (false, _) => format!("unavailable: {}", d.error.as_deref().unwrap_or("?")),
            }
        );
        for line in &d.ffmpeg_log {
            let _ = writeln!(s, "  {:<30} FFmpeg: {line}", "");
        }
    }
    if !r.vulkan_devices.is_empty() {
        let _ = writeln!(s, "\nVulkan devices");
        for d in &r.vulkan_devices {
            let kind = match d.device_type {
                VulkanDeviceType::DiscreteGpu => "discrete GPU",
                VulkanDeviceType::IntegratedGpu => "integrated GPU",
                VulkanDeviceType::VirtualGpu => "virtual GPU",
                VulkanDeviceType::Cpu => "CPU: no GPU acceleration",
                VulkanDeviceType::Other => "other",
            };
            let _ = writeln!(
                s,
                "  {}{}\n    {kind}; driver {}{}; Vulkan {}; vendor {} device {}",
                d.name,
                if d.used_by_ffmpeg {
                    "  (FFmpeg uses this one)"
                } else {
                    ""
                },
                d.driver.as_deref().unwrap_or("?"),
                d.driver_info
                    .as_ref()
                    .map(|i| format!(" ({i})"))
                    .unwrap_or_default(),
                d.api_version,
                d.vendor_id,
                d.device_id
            );
        }
    }
    let _ = writeln!(
        s,
        "\nDecode check: one {n}x{n} frame each, no fallback to software; the hardware's\n\
         picture must match the software decoder's",
        n = SAMPLE_SIZE
    );
    checks(&mut s, &r.decode_checks);
    let _ = writeln!(
        s,
        "\nEncode check: one {n}x{n} frame each on VA-API, decoded back in software",
        n = SAMPLE_SIZE
    );
    checks(&mut s, &r.encode_checks);
    if r.issues.is_empty() {
        let _ = writeln!(s, "\nNo problems found.");
    } else {
        let _ = writeln!(s, "\nProblems");
        for i in &r.issues {
            let tag = match i.severity {
                Severity::Error => "ERROR",
                Severity::Warning => "warning",
                Severity::Info => "note",
            };
            let _ = writeln!(
                s,
                "  [{tag}] {}\n    impact: {}\n    fix:",
                i.what, i.impact
            );
            for line in i.fix.lines() {
                let _ = writeln!(s, "      {}", line.trim());
            }
        }
    }
    s
}

/// A table of checks, one row per format and one column per path, with
/// each distinct reason as a numbered note below it.
fn checks(s: &mut String, checks: &[Check]) {
    let mut paths: Vec<&str> = Vec::new();
    let mut formats: Vec<&str> = Vec::new();
    for c in checks {
        if !paths.contains(&c.path.as_str()) {
            paths.push(&c.path);
        }
        if !formats.contains(&c.format.as_str()) {
            formats.push(&c.format);
        }
    }
    let mut row = format!("  {:<14}", "");
    for p in &paths {
        let _ = write!(row, " {p:<11}");
    }
    let _ = writeln!(s, "{}", row.trim_end());
    // Notes are a reason plus the first thing FFmpeg said, if anything,
    // without the "[hevc] " that would keep identical notes apart.
    let mut notes: Vec<String> = Vec::new();
    for f in &formats {
        let mut row = format!("  {f:<14}");
        for p in &paths {
            let Some(c) = checks.iter().find(|c| c.format == *f && c.path == *p) else {
                let _ = write!(row, " {:<11}", "");
                continue;
            };
            let word = match c.result {
                CheckResult::Works => "works",
                CheckResult::Fails => "FAILS",
                CheckResult::NotTested => "-",
            };
            let cell = match &c.reason {
                Some(reason) => {
                    let mut note = reason.clone();
                    if let Some(first) = c.ffmpeg_log.first() {
                        let msg = first
                            .strip_prefix('[')
                            .and_then(|l| l.split_once("] "))
                            .map_or(first.as_str(), |(_, m)| m);
                        let _ = write!(note, "\n      FFmpeg: {msg}");
                    }
                    let n = notes.iter().position(|x| *x == note).unwrap_or_else(|| {
                        notes.push(note);
                        notes.len() - 1
                    });
                    format!("{word} [{}]", n + 1)
                }
                None => word.to_string(),
            };
            let _ = write!(row, " {cell:<11}");
        }
        let _ = writeln!(s, "{}", row.trim_end());
    }
    for (i, note) in notes.iter().enumerate() {
        let _ = writeln!(s, "  [{}] {note}", i + 1);
    }
}
