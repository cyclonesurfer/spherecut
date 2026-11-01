//! What `spherecut doctor --file` says of a camera's file (M3 plan 5.4),
//! M3.2's unit (c): whether it can be stitched (its calibration is
//! there), whether both lenses decode on the path the viewer would take,
//! and, when not, what is missing.
//!
//! The built-in geometry table is M3.5's (plan 5.3) and the X5's fit
//! M3.6's, so every file's geometry is provisional here, and the summary
//! says so: "Insta360 X5 v1.1.22, geometry provisional".

use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;
use spherecut_core::TimeSpec;
use spherecut_media::Source;
use spherecut_media::backend::SeekMode;
use spherecut_media::probe::ProbeReport;

use crate::contract::{DecodeInfo, ErrorBody, SourceInfo, Warning};
use crate::still::{self, Failure, Still, Stop};

/// How sure a file's lens geometry is, until M3.5's table gives each
/// model and firmware its own (plan 5.3).
const GEOMETRY: &str = "provisional";

/// What `doctor --file` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileCheck {
    /// The file, as text: bytes that aren't UTF-8 are U+FFFD.
    pub path: String,
    /// The camera as probe names it: its model, with the vendor in front
    /// when the model doesn't start with it; null when probe names none.
    pub camera: Option<String>,
    /// The firmware the trailer gives, as written (`v1.1.22_build1`).
    pub firmware: Option<String>,
    /// Whether it can be stitched: two lens streams that decode, and the
    /// calibration the camera says it uses, in its trailer.
    pub stitchable: bool,
    /// That calibration's generation, `v6` on the X5, or null.
    pub calibration: Option<String>,
    /// Each lens stream, its first frame decoded on the chosen path; the
    /// first video stream alone for a file with no lens streams.
    pub lenses: Vec<LensCheck>,
    /// How sure its lenses' geometry is: `provisional` until M3.5's
    /// built-in table and M3.6's fit.
    pub geometry: &'static str,
    /// What keeps it from being stitched, in words; empty when nothing
    /// does.
    pub missing: Vec<String>,
    /// One line: the camera, its firmware and the geometry, such as
    /// "Insta360 X5 v1.1.22, geometry provisional".
    pub summary: String,
    /// What probing the file and opening its lenses warned of, such as
    /// damage, each once: frame's warnings, which doctor's envelope
    /// doesn't carry.
    pub warnings: Vec<Warning>,
}

/// A lens stream's first frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LensCheck {
    /// FFmpeg's stream index.
    pub stream: usize,
    /// Whether its first frame decoded.
    pub decodes: bool,
    /// The stream as its decoder opened it, or null when it didn't.
    pub source: Option<SourceInfo>,
    /// Where its first frame was decoded, or null when it wasn't.
    pub decode: Option<DecodeInfo>,
    /// Why not, when it didn't decode.
    pub error: Option<ErrorBody>,
}

/// Checks the file at `src`, each lens on the path `opts` chooses (its
/// decoder and render node; `equirect` and `stream` are set here). A file
/// that can't be probed is the probe's failure; one that probes is a
/// [`FileCheck`] whatever it says. It reads and decodes, so it runs off
/// the UI thread (D34).
pub fn check(src: Source<'_>, opts: &still::Options, stop: &Stop) -> Result<FileCheck, Failure> {
    let mut warnings = Vec::new();
    let any = still::Options {
        // Dual fisheye isn't a stitched equirect; doctor doesn't draw it.
        equirect: true,
        stream: None,
        ..opts.clone()
    };
    let probed = still::probe(src, &any, stop, &mut warnings)?;
    let r = &probed.report;
    let tracks = match &r.camera_file.lenses {
        Some(l) => l.tracks.to_vec(),
        None => vec![probed.stream.index],
    };
    let lenses: Vec<LensCheck> = tracks
        .into_iter()
        .map(|stream| {
            let one = still::Options {
                stream: Some(stream),
                ..any.clone()
            };
            lens(src, stream, &one, stop, &mut warnings)
        })
        .collect();
    let calibration = r.camera_file.calibration.as_ref();
    let in_use = calibration.and_then(|c| c.in_use.clone());
    let mut missing = Vec::new();
    if r.camera_file.records.is_none() {
        missing.push("a camera trailer: Spherecut stitches Insta360 dual-fisheye files".into());
    } else if r.camera_file.lenses.is_none() {
        missing.push(
            "two lens streams: an LRV holds both lenses in one picture, so stitch its master"
                .into(),
        );
    }
    if r.camera_file.records.is_some() {
        match calibration {
            None => missing.push("a calibration: the trailer holds none that parses".into()),
            Some(c) if c.in_use.is_none() => missing.push(format!(
                "the calibration in use: the trailer holds {} but doesn't say it uses one of them",
                c.generations.join(" and ")
            )),
            Some(_) => {}
        }
    }
    for l in lenses.iter().filter(|l| !l.decodes) {
        missing.push(format!(
            "stream #{}'s pictures: {}",
            l.stream,
            l.error
                .as_ref()
                .map_or("it doesn't decode", |e| e.message.as_str())
        ));
    }
    let camera = name(r);
    let firmware = r.camera.as_ref().and_then(|c| c.firmware.clone());
    let summary = format!(
        "{}{}, geometry {GEOMETRY}",
        camera.as_deref().unwrap_or("an unknown camera"),
        firmware
            .as_deref()
            .map(|f| format!(" {}", short_firmware(f)))
            .unwrap_or_default()
    );
    Ok(FileCheck {
        path: src.name.to_string_lossy().into_owned(),
        camera,
        firmware,
        stitchable: missing.is_empty() && lenses.len() == 2,
        calibration: in_use,
        lenses,
        geometry: GEOMETRY,
        missing,
        summary,
        warnings,
    })
}

/// Lens stream `stream`'s first frame, decoded as `opts` say, with what
/// decoding it warned of added to `warnings` unless they hold it already
/// (the file's probe gave its damage before).
fn lens(
    src: Source<'_>,
    stream: usize,
    opts: &still::Options,
    stop: &Stop,
    warnings: &mut Vec<Warning>,
) -> LensCheck {
    let mut mine = Vec::new();
    let out = first_frame(src, stream, opts, stop, &mut mine);
    for w in mine {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    out
}

fn first_frame(
    src: Source<'_>,
    stream: usize,
    opts: &still::Options,
    stop: &Stop,
    warnings: &mut Vec<Warning>,
) -> LensCheck {
    let mut out = LensCheck {
        stream,
        decodes: false,
        source: None,
        decode: None,
        error: None,
    };
    let opened = still::probe(src, opts, stop, warnings)
        .and_then(|probed| Still::open(src, &probed, None, opts, stop, warnings));
    let mut s = match opened {
        Ok(s) => s,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    out.source = Some(s.source_info());
    match s.seek(TimeSpec::default(), SeekMode::Exact, warnings) {
        Ok(first) => {
            out.decodes = true;
            out.decode = Some(s.decode_info(&first));
        }
        Err(e) => out.error = Some(e),
    }
    out
}

/// The camera's name as the agent's probe summary gives it: the model,
/// with the vendor in front when the model doesn't start with it.
fn name(r: &ProbeReport) -> Option<String> {
    let c = r.camera.as_ref()?;
    match (c.vendor.as_deref(), c.model.as_deref()) {
        (Some(v), Some(m)) if !m.starts_with(v) => Some(format!("{v} {m}")),
        (_, Some(m)) => Some(m.to_owned()),
        (Some(v), None) => Some(v.to_owned()),
        (None, None) => None,
    }
}

/// The firmware's version without the build after it: `v1.1.22` of
/// `v1.1.22_build1`.
fn short_firmware(f: &str) -> &str {
    f.split_once('_').map_or(f, |(v, _)| v)
}

/// The check as text, for people: what `spherecut doctor --file` adds
/// after the machine's report.
pub fn render_text(f: &FileCheck) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\nFile {}", Path::new(&f.path).display());
    let _ = writeln!(s, "  {}", f.summary);
    let _ = writeln!(
        s,
        "  can be stitched: {}",
        match (&f.calibration, f.stitchable) {
            (Some(g), true) => format!("yes, with its {g} calibration"),
            _ => "no".into(),
        }
    );
    for l in &f.lenses {
        let what = l
            .source
            .as_ref()
            .map(|s| {
                format!(
                    "{} {}x{} {}",
                    s.codec, s.size.width, s.size.height, s.pix_fmt
                )
            })
            .unwrap_or_else(|| "not opened".into());
        let how = match (&l.decode, &l.error) {
            (Some(d), _) => match &d.fallback {
                Some(f) => format!("decodes on {}, not on {}: {}", d.path, f.from, f.reason),
                None => format!("decodes on {}", d.path),
            },
            (None, Some(e)) => format!("doesn't decode: {}", e.message),
            (None, None) => "doesn't decode".into(),
        };
        let _ = writeln!(s, "  stream #{}  {what}, {how}", l.stream);
    }
    for m in &f.missing {
        let _ = writeln!(s, "  missing: {m}");
    }
    s
}
