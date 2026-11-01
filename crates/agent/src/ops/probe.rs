//! `probe` (a1-plan section 6.1, amendments 19 and 31): what media files
//! are, a few hundred bytes each, or a folder's clips a page at a time.
//!
//! A summary is built from media's `ProbeReport`; `detail: "full"` adds
//! the report itself, as `spherecut probe --json` has it, cut to fit the
//! result. Every string read from a file goes through [`text::field`]. A
//! file that fails is an entry with `ok: false` and its error rather than
//! a failed call, so that the other files' entries survive, and the error
//! names the argument (`files[2]`), never the file.
//!
//! A call that reaches its deadline returns what it finished: a list of
//! files says `complete: false` and names the rest in `not_done`, by
//! index, and a folder's page ends early with a `next_cursor` from there.
//! So does a result that would pass [`RESULT_CHARS`].

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use spherecut_media::camera::{companion_path, verify_companion};
use spherecut_media::detect::Layout;
use spherecut_media::probe::{ProbeError, ProbeReport, StreamInfo, StreamKind};
use spherecut_media::spherical::{Projection, StereoMode};

use super::Ctx;
use super::clips::{self, Clip};
use crate::error::{ToolError, codes};
use crate::limits::{FOLDERS_CHARS, FULL_PER_CALL, MAX_PAGE_SIZE, PAGE_SIZE, RESULT_CHARS};
use crate::text;

/// How much `probe` says about each file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    #[default]
    Summary,
    /// The summary and the whole `ProbeReport`.
    Full,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub files: Option<Vec<PathBuf>>,
    pub folder: Option<PathBuf>,
    #[serde(default)]
    pub detail: Detail,
    pub page_size: Option<usize>,
    pub cursor: Option<String>,
}

/// What a result keeps aside for everything but its entries: the
/// folder's subfolders and the warnings.
const RESERVED_CHARS: usize = FOLDERS_CHARS + 2_000;

/// A summary's share of a full entry, the report having the rest.
const SUMMARY_CHARS: usize = 1_500;

pub fn run(req: Request, cx: &Ctx) -> Result<Value, ToolError> {
    let Request {
        files,
        folder,
        detail,
        page_size,
        cursor,
    } = req;
    match (files, folder) {
        (Some(_), Some(_)) => Err(ToolError::new(codes::BAD_ARGUMENT, "can't come with files")
            .at("folder")
            .valid("files, or folder, not both")
            .next("call probe once for the files and once for the folder")),
        (None, None) => Err(
            ToolError::new(codes::BAD_ARGUMENT, "probe needs files or a folder")
                .valid("files: a list of paths, or folder: one path"),
        ),
        (Some(files), None) => {
            for (arg, given) in [
                ("page_size", page_size.is_some()),
                ("cursor", cursor.is_some()),
            ] {
                if given {
                    return Err(ToolError::new(codes::BAD_ARGUMENT, "is only for a folder")
                        .at(arg)
                        .valid("leave it out with files"));
                }
            }
            files_of(&files, detail, cx)
        }
        (None, Some(folder)) => folder_of(&folder, detail, page_size, cursor.as_deref(), cx),
    }
}

/// One file to probe: its argument, the path to open, and what its entry
/// starts with.
struct Job {
    arg: String,
    path: PathBuf,
    entry: Map<String, Value>,
}

/// Why a call stopped before its last file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Deadline,
    Full,
}

/// The entries made, and the index of the first not done and why, when
/// the call stopped short.
type Probed = (Vec<Value>, Option<(usize, Stop)>);

/// The entries of `jobs` in order, each made by `entry` with its share
/// of `room`, until the deadline or until the next would take the entries
/// past `room` characters; then the index of the first not done, and
/// why. An error only when nothing was done: the call's own
/// (`timed_out`), or its cancel.
fn probe_all(
    jobs: Vec<Job>,
    room: usize,
    cx: &Ctx,
    mut entry: impl FnMut(Job, usize) -> Value,
) -> Result<Probed, ToolError> {
    let total = jobs.len();
    // A full report's share of the room, so that every file gets one.
    let share = (room / total.max(1)).saturating_sub(SUMMARY_CHARS);
    let mut entries = Vec::with_capacity(total);
    let mut used = 0;
    for (i, job) in jobs.into_iter().enumerate() {
        if let Some(stop) = stop_or(cx, i)? {
            return Ok((entries, Some((i, stop))));
        }
        cx.call
            .progress(i as f64, Some(total as f64), Some("probing"));
        let made = entry(job, share);
        // A probe the call's stop cut short is no answer for its file.
        if let Some(stop) = stop_or(cx, i)? {
            return Ok((entries, Some((i, stop))));
        }
        let len = made.to_string().chars().count() + 1;
        if used + len > room && i > 0 {
            return Ok((entries, Some((i, Stop::Full))));
        }
        used += len;
        entries.push(made);
    }
    Ok((entries, None))
}

/// Whether the call has stopped at its deadline (`Some`) with `done`
/// files to show for it; an error when it was cancelled, or when it timed
/// out with nothing done.
fn stop_or(cx: &Ctx, done: usize) -> Result<Option<Stop>, ToolError> {
    match cx.call.check() {
        Ok(()) => Ok(None),
        Err(e) if e.code == codes::TIMED_OUT && done > 0 => Ok(Some(Stop::Deadline)),
        Err(e) => Err(e),
    }
}

/// A file's entry: its summary (and report), or its error.
fn one(job: Job, detail: Detail, share: usize, cx: &Ctx) -> Value {
    let Job {
        arg,
        path,
        mut entry,
    } = job;
    let failed = |mut entry: Map<String, Value>, e: ToolError| {
        entry.insert("ok".into(), json!(false));
        entry.insert(
            "error".into(),
            serde_json::to_value(&e).unwrap_or(Value::Null),
        );
        Value::Object(entry)
    };
    let opened = match cx.roots.open_file(&arg, &path) {
        Ok(o) => o,
        Err(e) => return failed(entry, e),
    };
    match spherecut_media::probe::probe_cancellable(opened.source(), cx.call.interrupt()) {
        Ok(mut report) => {
            // An Insta360 master's LRV, or an LRV's master, opened beneath
            // the roots as the file was (D45). One that isn't there, or
            // isn't beneath a root, leaves `companion` out.
            if let Some(other) = companion_path(&report)
                && let Ok(o) = cx.roots.open_file(&arg, &other)
            {
                let c = verify_companion(&report, opened.source().open, o.source());
                report.camera_file.companion = Some(c);
            }
            summary(&mut entry, &report);
            if detail == Detail::Full {
                let (report, cut) = full_report(&report, share);
                entry.insert("report".into(), report);
                if cut {
                    entry.insert("report_cut".into(), json!(true));
                }
            }
            Value::Object(entry)
        }
        Err(e) => {
            if detail == Detail::Full {
                let mut log = json!(e.ffmpeg_log);
                text::untrusted(&mut log);
                entry.insert("ffmpeg_log".into(), log);
            }
            failed(entry, media_error(&arg, &e, detail))
        }
    }
}

/// A failure to open or probe a file, by its media code, as A1's code
/// and words for the file's argument (amendment 28); `look` uses them
/// too.
pub(crate) fn words(code: &str) -> (&'static str, &'static str) {
    match code {
        "not_media" | "unsupported_container" | "nested_open_refused" => (
            codes::UNSUPPORTED_CONTAINER,
            "is empty, or isn't MP4, MOV, Matroska or WebM",
        ),
        "truncated" => (codes::DAMAGED, "is cut short: it ends inside its structure"),
        "damaged" | "open_failed" => (codes::DAMAGED, "is damaged: FFmpeg can't open it"),
        "not_found" => (codes::NOT_FOUND, "doesn't exist"),
        "not_a_file" => (codes::NOT_A_FILE, "isn't a regular file"),
        "io_error" => (codes::DAMAGED, "can't be read: an I/O error"),
        _ => (codes::DAMAGED, "can't be probed"),
    }
}

/// A probe's failure as the agent's error: A1's codes (amendment 28),
/// and never FFmpeg's words, which `detail: "full"` gives as
/// `ffmpeg_log`.
fn media_error(arg: &str, e: &ProbeError, detail: Detail) -> ToolError {
    let (code, message) = words(e.code());
    let next = match detail {
        Detail::Summary => "call probe on it with detail \"full\" for FFmpeg's own words",
        Detail::Full => "ffmpeg_log has FFmpeg's own words",
    };
    ToolError::new(code, message).at(arg).next(next)
}

/// The video streams a summary describes: not cover art.
fn video_streams(r: &ProbeReport) -> Vec<&StreamInfo> {
    r.streams
        .iter()
        .filter(|s| {
            s.kind == StreamKind::Video
                && s.video.is_some()
                && !s.disposition.iter().any(|d| d == "attached_pic")
        })
        .collect()
}

/// A file's summary into `entry`: about 300 bytes for a plain file, and
/// about 1 KB for a camera's with its trailer (an X5 master's is 944).
fn summary(entry: &mut Map<String, Value>, r: &ProbeReport) {
    let video = video_streams(r);
    let v = video.first().copied();
    let info = v.and_then(|s| s.video.as_ref());
    let mut put = |k: &str, v: Value| {
        entry.insert(k.into(), v);
    };
    put("ok", json!(r.failure().is_none()));
    let duration = r.container.duration.or_else(|| v.and_then(|s| s.duration));
    put(
        "duration",
        serde_json::to_value(duration).unwrap_or(Value::Null),
    );
    put(
        "size",
        info.map_or(Value::Null, |i| json!([i.width, i.height])),
    );
    if video.len() > 1 {
        put("video_streams", json!(video.len()));
    }
    let (fps, vfr) = rate(v);
    put("fps", fps.map_or(Value::Null, |f| json!(f)));
    if vfr {
        put("vfr", json!(true));
    }
    put("codec", opt_field(v.and_then(|s| s.codec.as_deref())));
    put(
        "bit_depth",
        info.and_then(|i| i.bit_depth)
            .map_or(Value::Null, |b| json!(b)),
    );
    // A file without video has no projection.
    let (projection, metadata) = match v {
        Some(_) => {
            let (p, m) = projection(r, v);
            (json!(p), json!(m))
        }
        None => (Value::Null, Value::Null),
    };
    put("projection", projection);
    put("metadata", metadata);
    let stereo = v
        .and_then(|s| s.spherical.v2.as_ref())
        .and_then(|v2| v2.stereo)
        .filter(|s| *s != StereoMode::Mono);
    if let Some(stereo) = stereo {
        put("stereo", json!(stereo.name()));
    }
    put("camera", camera(r));
    if let Some(detail) = camera_detail(r) {
        put("camera_detail", detail);
    }
    // What an Insta360 trailer says (M3 plan 5.4), each only when there.
    let f = &r.camera_file;
    let mut some = |k: &str, v: Value| {
        if !v.is_null() {
            put(k, v);
        }
    };
    some("lenses", json!(f.lenses));
    some("calibration", json!(f.calibration));
    some(
        "companion",
        f.companion.as_ref().map_or(Value::Null, |c| {
            json!({
                "path": text::field(&c.path.to_string_lossy()),
                "verified": c.verified,
                "why": c.why.as_deref().map(text::field),
            })
        }),
    );
    some("exposure_entries", json!(f.exposure_entries));
    some("records", json!(f.records));
    put("audio", audio(r));
    put("problems", problems(r));
}

fn opt_field(s: Option<&str>) -> Value {
    s.map_or(Value::Null, |s| json!(text::field(s)))
}

/// The first video stream's frame rate, to 3 decimals, and whether it is
/// variable: FFmpeg's average and its base rate differ by more than 1 %.
fn rate(v: Option<&StreamInfo>) -> (Option<f64>, bool) {
    let positive = |r: Option<spherecut_core::Rational>| {
        r.map(|r| r.to_f64()).filter(|f| f.is_finite() && *f > 0.0)
    };
    let Some(s) = v else {
        return (None, false);
    };
    let (avg, base) = (positive(s.avg_frame_rate), positive(s.r_frame_rate));
    let fps = avg.or(base).map(|f| (f * 1000.0).round() / 1000.0);
    let vfr = matches!((avg, base), (Some(a), Some(b)) if (a - b).abs() > b * 0.01);
    (fps, vfr)
}

/// What the picture is, and what says so: the spherical boxes, the
/// camera's own layout (its metadata), or nothing.
fn projection(r: &ProbeReport, v: Option<&StreamInfo>) -> (&'static str, &'static str) {
    if let Some(s) = v {
        if let Some(p) = s
            .spherical
            .v2
            .as_ref()
            .and_then(|v2| v2.projection.as_ref())
        {
            let p = match p {
                Projection::Equirect { .. } => "equirect",
                Projection::Cubemap { .. } => "cubemap",
                Projection::Mesh | Projection::Other { .. } => "unknown",
            };
            return (p, "spherical_v2");
        }
        if let Some(v1) = s.spherical.v1.as_ref().filter(|v1| v1.spherical) {
            let kind = v1.projection_type.as_deref().map(str::to_ascii_lowercase);
            let p = match kind.as_deref() {
                None | Some("equirectangular") => "equirect",
                Some("cubemap") => "cubemap",
                Some(_) => "unknown",
            };
            return (p, "spherical_v1");
        }
    }
    let Some(c) = &r.camera else {
        return ("flat", "none");
    };
    match c.layout {
        Layout::Insta360DualFisheye | Layout::DjiDualFisheye => ("dual_fisheye", "vendor"),
        Layout::GoProEac => ("cubemap", "vendor"),
        // Insta360's and the GoPro MAX's proxies are dual fisheye like
        // their masters (insta360.md, gopro.md 5); DJI's is disputed
        // (dji.md 5).
        Layout::LowResCompanion if matches!(c.vendor.as_deref(), Some("Insta360" | "GoPro")) => {
            ("dual_fisheye", "vendor")
        }
        Layout::LowResCompanion => ("unknown", "none"),
        Layout::EquirectLikely => ("equirect", "none"),
        // Tagged on a stream other than the first video one.
        Layout::EquirectTagged => ("equirect", "spherical_v2"),
        Layout::Unknown => ("flat", "none"),
    }
}

/// The camera's name, as [`spherecut_media::detect::CameraGuess::name`]
/// gives it: its model, with the vendor in front when the model doesn't
/// start with it.
fn camera(r: &ProbeReport) -> Value {
    opt_field(r.camera.as_ref().and_then(|c| c.name()).as_deref())
}

/// The camera as the file names it, beside `camera`'s one string (M3
/// plan 5.4); None, and the summary leaves it out, when `camera` is null:
/// a file whose layout was guessed but that names no vendor or model.
fn camera_detail(r: &ProbeReport) -> Option<Value> {
    let c = r.camera.as_ref()?;
    if c.vendor.is_none() && c.model.is_none() {
        return None;
    }
    let s = |v: &Option<String>| opt_field(v.as_deref());
    Some(json!({
        "vendor": s(&c.vendor),
        "model": s(&c.model),
        "firmware": s(&c.firmware),
        "serial": s(&c.serial),
    }))
}

/// The first audio stream: `"stereo, 48 kHz"`, `"ambiX 1st order, 4
/// channels, 48 kHz"`, with how many more there are; null for none.
fn audio(r: &ProbeReport) -> Value {
    let streams: Vec<_> = r
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .filter_map(|s| s.audio.as_ref().map(|a| (s, a)))
        .collect();
    let Some((s, a)) = streams.first() else {
        return Value::Null;
    };
    let channels = match s
        .spherical
        .spatial_audio
        .as_ref()
        .filter(|sa| sa.is_ambix())
    {
        Some(sa) => format!(
            "ambiX {} order, {} channels",
            ordinal(sa.ambisonic_order),
            a.channels
        ),
        None => match a.channels {
            1 => "mono".into(),
            2 => "stereo".into(),
            n => format!("{n} channels"),
        },
    };
    let khz = f64::from(a.sample_rate) / 1000.0;
    let more = match streams.len() - 1 {
        0 => String::new(),
        1 => " (and 1 more audio stream)".into(),
        n => format!(" (and {n} more audio streams)"),
    };
    json!(format!("{channels}, {khz} kHz{more}"))
}

pub(crate) fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The codes of what probe found wrong, each once: damage, warnings, and
/// why the file fails.
fn problems(r: &ProbeReport) -> Value {
    let mut found: Vec<&str> = Vec::new();
    let failure = r.failure();
    for code in r
        .damage
        .iter()
        .chain(&r.warnings)
        .chain(failure.as_ref())
        .map(|p| p.code)
    {
        if !found.contains(&code) {
            found.push(code);
        }
    }
    json!(found)
}

/// The whole report, its strings through [`text::field`], cut until it
/// fits in `share` characters: lists and tags shortened, then strings.
/// Null when even the shortest doesn't fit. The flag says it was cut.
fn full_report(r: &ProbeReport, share: usize) -> (Value, bool) {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    text::untrusted(&mut v);
    if chars(&v) <= share {
        return (v, false);
    }
    for (items, bytes) in [(32, 512), (8, 128), (2, 32)] {
        let mut cut = v.clone();
        shorten(&mut cut, items, bytes);
        if chars(&cut) <= share {
            return (cut, true);
        }
    }
    (Value::Null, true)
}

fn chars(v: &Value) -> usize {
    v.to_string().chars().count()
}

/// Every list in `v` to `items` entries, every `tags` map to `items`
/// members, and every string to `bytes`, ending in `…` where cut.
fn shorten(v: &mut Value, items: usize, bytes: usize) {
    match v {
        Value::String(s) if s.len() > bytes => {
            let mut end = bytes.saturating_sub('…'.len_utf8());
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s.push('…');
        }
        Value::Array(a) => {
            a.truncate(items);
            a.iter_mut().for_each(|x| shorten(x, items, bytes));
        }
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if k == "tags"
                    && let Value::Object(tags) = x
                {
                    let keep: Map<String, Value> =
                        std::mem::take(tags).into_iter().take(items).collect();
                    *tags = keep;
                }
                shorten(x, items, bytes);
            }
        }
        _ => {}
    }
}

fn warning(code: &str, message: String) -> Value {
    json!({"code": code, "message": message})
}

/// `files`: an entry each, in order.
fn files_of(files: &[PathBuf], detail: Detail, cx: &Ctx) -> Result<Value, ToolError> {
    if detail == Detail::Full && files.len() > FULL_PER_CALL {
        return Err(ToolError::new(
            codes::OUT_OF_RANGE,
            format!("has {} files, past what detail \"full\" takes", files.len()),
        )
        .at("files")
        .valid(format!("1 to {FULL_PER_CALL} files with detail \"full\""))
        .next("probe the files a few at a time, or with detail \"summary\""));
    }
    let jobs = files
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let mut entry = Map::new();
            entry.insert("file".into(), json!(text::field(&path.to_string_lossy())));
            Job {
                arg: format!("files[{i}]"),
                path: path.clone(),
                entry,
            }
        })
        .collect();
    let (entries, stopped) = probe_all(jobs, RESULT_CHARS - RESERVED_CHARS, cx, |job, share| {
        one(job, detail, share, cx)
    })?;
    let mut warnings = Vec::new();
    let mut out = json!({"files": entries, "complete": stopped.is_none()});
    if let Some((first, why)) = stopped {
        out["not_done"] = json!((first..files.len()).collect::<Vec<_>>());
        warnings.push(match why {
            Stop::Deadline => warning(
                "deadline",
                format!(
                    "the call's time ran out after {first} of {} files; not_done lists the rest, by index",
                    files.len()
                ),
            ),
            Stop::Full => warning(
                "result_full",
                format!(
                    "the result was full after {first} of {} files; not_done lists the rest, by index",
                    files.len()
                ),
            ),
        });
    }
    if cut_any(&out["files"]) {
        warnings.push(cut_warning());
    }
    out["warnings"] = Value::Array(warnings);
    Ok(out)
}

fn cut_any(entries: &Value) -> bool {
    entries
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| e["report_cut"] == true)
}

fn cut_warning() -> Value {
    warning(
        "report_cut",
        format!(
            "reports marked report_cut were shortened to fit {RESULT_CHARS} characters (lists, tags and strings cut, ending in …; a null report didn't fit at all); probe fewer files for more"
        ),
    )
}

/// `folder`: a page of its clips.
fn folder_of(
    folder: &Path,
    detail: Detail,
    page_size: Option<usize>,
    cursor: Option<&str>,
    cx: &Ctx,
) -> Result<Value, ToolError> {
    let most = match detail {
        Detail::Summary => MAX_PAGE_SIZE,
        Detail::Full => FULL_PER_CALL,
    };
    let size = page_size.unwrap_or(match detail {
        Detail::Summary => PAGE_SIZE,
        Detail::Full => FULL_PER_CALL,
    });
    if size > most {
        return Err(ToolError::new(codes::OUT_OF_RANGE, format!("is {size}"))
            .at("page_size")
            .valid(format!("1 to {most} with detail \"full\""))
            .next("leave page_size out, or use detail \"summary\""));
    }
    let listing = cx.roots.list("folder", folder)?;
    let fingerprint = clips::fingerprint(&listing);
    let grouped = clips::group(&listing);
    let total = grouped.clips.len();
    let start = match cursor {
        None => 0,
        Some(c) => from_cursor(c, fingerprint, total)?,
    };
    let page = &grouped.clips[start..(start + size).min(total)];
    let jobs = page.iter().map(|c| clip_job(folder, c)).collect();
    let (folders, folders_left_out) = folder_names(&grouped.folders);
    let (entries, stopped) = probe_all(jobs, RESULT_CHARS - RESERVED_CHARS, cx, |job, share| {
        one(job, detail, share, cx)
    })?;
    let next = start + entries.len();
    let mut warnings = Vec::new();
    if let Some((_, why)) = stopped {
        let cause = match why {
            Stop::Deadline => "the call's time ran out",
            Stop::Full => "the result was full",
        };
        warnings.push(warning(
            if why == Stop::Deadline {
                "deadline"
            } else {
                "result_full"
            },
            format!(
                "{cause} after {} clips of this page; next_cursor goes on from there",
                entries.len()
            ),
        ));
    }
    if listing.left_out > 0 {
        warnings.push(warning(
            "entries_left_out",
            format!(
                "the folder has {} more entries than the {} probe reads; move clips to subfolders to see them all",
                listing.left_out,
                listing.entries.len()
            ),
        ));
    }
    let entries = Value::Array(entries);
    if cut_any(&entries) {
        warnings.push(cut_warning());
    }
    Ok(json!({
        "folder": text::field(&folder.to_string_lossy()),
        "clips": entries,
        "total": total,
        "folders": folders,
        "folders_left_out": folders_left_out,
        "skipped": grouped.skipped,
        "entries_left_out": listing.left_out,
        "next_cursor": (next < total).then(|| format!("{next}:{fingerprint:016x}")),
        "warnings": warnings,
    }))
}

/// A clip's job: its primary file, and an entry that starts with the
/// name, when it was recorded, and its files by role.
fn clip_job(folder: &Path, clip: &Clip) -> Job {
    let mut entry = Map::new();
    let name = clip.primary();
    entry.insert("file".into(), json!(text::field(&name.to_string_lossy())));
    entry.insert("recorded".into(), json!(clip.recorded));
    entry.insert("files".into(), clip.files_json());
    Job {
        arg: "folder".into(),
        path: folder.join(name),
        entry,
    }
}

/// Subfolder names until [`FOLDERS_CHARS`], and how many more there are.
fn folder_names(names: &[std::ffi::OsString]) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut used = 0;
    for (i, n) in names.iter().enumerate() {
        let name = text::field(&n.to_string_lossy());
        used += json!(name).to_string().chars().count() + 1;
        if used > FOLDERS_CHARS {
            return (out, names.len() - i);
        }
        out.push(name);
    }
    (out, 0)
}

/// Where a cursor says the page starts: `<offset>:<fingerprint>`, the
/// fingerprint hex. A folder whose fingerprint has changed is stale.
fn from_cursor(cursor: &str, fingerprint: u64, total: usize) -> Result<usize, ToolError> {
    let bad = || {
        ToolError::new(codes::BAD_ARGUMENT, "isn't a cursor probe gave")
            .at("cursor")
            .next("pass a page's next_cursor as it came, or leave cursor out for the first page")
    };
    let (offset, print) = cursor.split_once(':').ok_or_else(bad)?;
    let offset: usize = offset.parse().map_err(|_| bad())?;
    let print = u64::from_str_radix(print, 16).map_err(|_| bad())?;
    if print != fingerprint || offset >= total.max(1) {
        return Err(ToolError::new(
            codes::CURSOR_STALE,
            "the folder has changed since that page",
        )
        .at("cursor")
        .next("call probe on the folder again without cursor"));
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::roots::Roots;
    use crate::rpc::CallCx;

    fn parts() -> (Roots, CallCx) {
        let deadline = Instant::now() + Duration::from_secs(50);
        (Roots::new(&[], None).unwrap(), CallCx::standalone(deadline))
    }

    fn jobs(n: usize) -> Vec<Job> {
        (0..n)
            .map(|i| Job {
                arg: format!("files[{i}]"),
                path: PathBuf::from("x"),
                entry: Map::new(),
            })
            .collect()
    }

    #[test]
    fn the_deadline_keeps_what_was_done() {
        let (roots, call) = parts();
        let cx = Ctx {
            roots: &roots,
            call: &call,
            image_budget: crate::limits::INLINE_IMAGE_BYTES,
        };
        let mut n = 0;
        let (entries, stopped) = probe_all(jobs(5), 10_000, &cx, |_, _| {
            n += 1;
            if n == 2 {
                call.expire();
            }
            json!({ "n": n })
        })
        .unwrap();
        // The second was being made as the time ran out: it isn't kept.
        assert_eq!(entries, [json!({"n": 1})]);
        assert_eq!(stopped, Some((1, Stop::Deadline)));
    }

    #[test]
    fn a_deadline_with_nothing_done_is_the_call_s() {
        let (roots, call) = parts();
        let cx = Ctx {
            roots: &roots,
            call: &call,
            image_budget: crate::limits::INLINE_IMAGE_BYTES,
        };
        let e = probe_all(jobs(3), 10_000, &cx, |_, _| {
            call.expire();
            json!({})
        })
        .unwrap_err();
        assert_eq!(e.code, codes::TIMED_OUT);
    }

    #[test]
    fn a_cancel_is_never_a_partial_result() {
        let (roots, call) = parts();
        let cx = Ctx {
            roots: &roots,
            call: &call,
            image_budget: crate::limits::INLINE_IMAGE_BYTES,
        };
        let mut n = 0;
        let e = probe_all(jobs(3), 10_000, &cx, |_, _| {
            n += 1;
            if n == 2 {
                call.canceller().cancel();
            }
            json!({})
        })
        .unwrap_err();
        assert_eq!(e.code, codes::CANCELLED);
    }

    #[test]
    fn a_full_result_ends_before_the_entry_that_would_not_fit() {
        let (roots, call) = parts();
        let cx = Ctx {
            roots: &roots,
            call: &call,
            image_budget: crate::limits::INLINE_IMAGE_BYTES,
        };
        // 49 characters and a comma each.
        let entry = json!({ "s": "x".repeat(41) });
        let (entries, stopped) = probe_all(jobs(5), 120, &cx, |_, _| entry.clone()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(stopped, Some((2, Stop::Full)));
        // The first entry is kept whatever its size.
        let (entries, _) = probe_all(jobs(5), 10, &cx, |_, _| entry.clone()).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn ordinals() {
        let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101, 111]
            .into_iter()
            .map(ordinal)
            .collect();
        assert_eq!(
            got,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "101st",
                "111th"
            ]
        );
    }

    #[test]
    fn cursors() {
        let fp = 0x0123_4567_89ab_cdef;
        assert_eq!(from_cursor("20:0123456789abcdef", fp, 45), Ok(20));
        assert_eq!(from_cursor("0:0123456789abcdef", fp, 0), Ok(0));
        for bad in [
            "",
            "20",
            "x:0123456789abcdef",
            "20:xyz",
            "-1:0123456789abcdef",
        ] {
            let e = from_cursor(bad, fp, 45).err().map(|e| e.code);
            assert_eq!(e, Some(codes::BAD_ARGUMENT), "{bad}");
        }
        for stale in [
            "20:0123456789abcdee",
            "45:0123456789abcdef",
            "90:0123456789abcdef",
        ] {
            let e = from_cursor(stale, fp, 45).err().map(|e| e.code);
            assert_eq!(e, Some(codes::CURSOR_STALE), "{stale}");
        }
    }

    #[test]
    fn shortening_cuts_lists_tags_and_strings() {
        let mut v = json!({
            "a": [1, 2, 3, 4],
            "tags": {"k1": "v", "k2": "v", "k3": "v"},
            "s": "é".repeat(40),
            "nested": {"tags": {"x": 1, "y": 2}, "list": ["abcdefghi", "b", "c"]},
        });
        shorten(&mut v, 2, 8);
        assert_eq!(v["a"], json!([1, 2]));
        assert_eq!(v["tags"].as_object().map(Map::len), Some(2));
        let s = v["s"].as_str().unwrap_or_default();
        assert!(s.len() <= 8 && s.ends_with('…'), "{s}");
        assert_eq!(v["nested"]["list"], json!(["abcde…", "b"]));
    }

    #[test]
    fn folder_names_stop_at_their_share() {
        let names: Vec<std::ffi::OsString> = (0..2_000)
            .map(|i| format!("folder {i:04}").into())
            .collect();
        let (listed, left) = folder_names(&names);
        assert_eq!(listed.len() + left, names.len());
        assert!(left > 0);
        let chars: usize = listed.iter().map(|n| n.len() + 3).sum();
        assert!(chars <= FOLDERS_CHARS, "{chars}");
        assert_eq!(
            folder_names(&names[..3]),
            (
                vec![
                    "folder 0000".into(),
                    "folder 0001".into(),
                    "folder 0002".into()
                ],
                0
            )
        );
    }
}
