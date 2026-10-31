//! `compare` (a2-plan 7.1, D59): how alike two videos are, or a video
//! and the render a project makes, at sampled times: PSNR and SSIM of
//! their BT.709 luma as an encoder codes it (`render::compare`, the
//! export's parity tests' code), the worst time of each, and the means;
//! with `sheet`, the worst times side by side with their difference.
//!
//! Each time is decoded exactly in each file and drawn whole at the
//! file's size in 16 bits a channel; a project's frame is rendered at
//! that size as the export renders it ([`sequence`]). A file's times are
//! its own; a project's frame is the one its render shows at the file's
//! frame's time, from `from` (the trim's start by default), so a render
//! of the project compares with the project frame for frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use spherecut_core::{FrameTime, Rational, Time, TimeSpec};
use spherecut_media::backend::SeekMode;
use spherecut_render::compare::{Plane, psnr_plane, ssim_plane};
use spherecut_render::{Depth, Filter, Gpu, Image, Pixels, ViewParams};
use spherecut_sphere::orient::Ypr;

use super::look::{self, TimeArg};
use super::project::at;
use super::{Ctx, sequence};
use crate::contract::Warning;
use crate::error::{ToolError, codes};
use crate::image::{self as picture, Canvas};
use crate::limits::{
    COMPARE_COUNT, COMPARE_RESERVE, COMPARE_TIMES, DIFF_GAIN, IMAGE_DEFAULT_EDGE, SHEET_ROWS,
};
use crate::roots::Opened;
use crate::still::{self, Decoded, Draw, Failure, Still, seconds};
use crate::{rpc, text};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: PathBuf,
    pub against_file: Option<PathBuf>,
    pub against_project: Option<PathBuf>,
    /// Where in the clip's media the project's render starts.
    pub from: Option<TimeArg>,
    pub times: Option<Vec<TimeArg>>,
    pub count: Option<u32>,
    #[serde(default)]
    pub sheet: bool,
}

fn bad(arg: &str, message: impl Into<String>) -> ToolError {
    ToolError::new(codes::BAD_ARGUMENT, message).at(arg)
}

/// What `file` is compared with.
#[expect(clippy::large_enum_variant, reason = "one for a call")]
enum Against<'g> {
    File {
        path: PathBuf,
        _opened: Opened,
        still: Still<'g>,
    },
    Project {
        path: PathBuf,
        loaded: sequence::Loaded,
        frames: sequence::Frames,
        renderer: sequence::Renderer<'g>,
    },
}

/// One time measured.
struct Measured {
    /// Its index in the request's times.
    index: usize,
    time: FrameTime,
    against: Value,
    psnr: f64,
    ssim: f64,
    /// The file, the other and their difference, at the sheet's tile
    /// size.
    thumbs: Option<[Image; 3]>,
}

pub fn run(req: Request, cx: &Ctx) -> Result<(Value, Vec<rpc::Image>), ToolError> {
    match (&req.against_file, &req.against_project) {
        (Some(_), Some(_)) => {
            return Err(bad("against_project", "can't come with against_file")
                .valid("against_file or against_project, one of them"));
        }
        (None, None) => {
            return Err(bad("against_file", "is missing")
                .valid("against_file, another video, or against_project, a .spherecut file"));
        }
        _ => {}
    }
    if req.from.is_some() && req.against_project.is_none() {
        return Err(bad("from", "is where a project's render starts")
            .valid("from with against_project only"));
    }
    if req.times.is_some() && req.count.is_some() {
        return Err(bad("count", "can't come with times").valid("times, or count, not both"));
    }
    let given = req
        .times
        .as_ref()
        .map(|ts| {
            ts.iter()
                .enumerate()
                .map(|(i, t)| look::time(&format!("times[{i}]"), t))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    cx.call.check()?;

    let gpu = look::gpu()?;
    let stop = still::Stop::new(cx.call.token(), cx.call.interrupt());
    let mut warnings: Vec<Warning> = Vec::new();
    let (opened, mut file) = open_flat(cx, "file", &req.file, &gpu, &stop, &mut warnings)?;
    let info = file.info().clone();
    let size = info.size;
    let mut against = match (&req.against_file, &req.against_project) {
        (Some(p), _) => {
            let (o, still) = open_flat(cx, "against_file", p, &gpu, &stop, &mut warnings)?;
            let theirs = still.info().size;
            if theirs != size {
                return Err(bad(
                    "against_file",
                    format!(
                        "is {}x{}, and file {}x{}",
                        theirs.0, theirs.1, size.0, size.1
                    ),
                )
                .valid("two videos of one size")
                .next("compare a project with the file instead: it renders at the file's size"));
            }
            Against::File {
                path: p.clone(),
                _opened: o,
                still,
            }
        }
        (None, Some(p)) => {
            let loaded = sequence::load(cx, "against_project", p)?;
            let from = req
                .from
                .as_ref()
                .map(|t| at("from", t, Some(loaded.media.video.rate)))
                .transpose()?;
            let frames = sequence::Frames::new(&loaded.project, from, "from")?;
            let ours = f64::from(size.0) / f64::from(size.1.max(1));
            if (ours / frames.aspect() - 1.0).abs() > 0.01 {
                let (w, h) = loaded.project.the_sequence().output.size();
                return Err(bad(
                    "against_project",
                    format!(
                        "renders {w}x{h}, a shape other than file's {}x{}",
                        size.0, size.1
                    ),
                )
                .valid("a project whose output has file's shape"));
            }
            let renderer = sequence::Renderer::open(&loaded, &gpu, &stop, &mut warnings)
                .map_err(|f| failed(cx, &f, "against_project"))?;
            Against::Project {
                path: p.clone(),
                loaded,
                frames,
                renderer,
            }
        }
        (None, None) => return Err(ToolError::internal("nothing to compare against")),
    };

    // The times, in file time.
    let specs: Vec<(String, TimeSpec)> = match given {
        Some(g) => g
            .into_iter()
            .enumerate()
            .map(|(i, s)| (format!("times[{i}]"), s))
            .collect(),
        None => {
            let count = req.count.unwrap_or(COMPARE_COUNT);
            let ours = info.duration;
            let theirs = match &against {
                Against::File { still, .. } => still.info().duration,
                Against::Project { frames, .. } => Some(frames.duration()),
            };
            let Some(d) = ours.into_iter().chain(theirs).min() else {
                return Err(
                    bad("times", "neither video says how long it is").valid("times to compare at")
                );
            };
            spread(d, count)?
                .into_iter()
                .map(|t| ("count".to_owned(), TimeSpec::Time(t)))
                .collect()
        }
    };
    if specs.len() > COMPARE_TIMES {
        return Err(ToolError::new(
            codes::OUT_OF_RANGE,
            format!(
                "{} times, past the {COMPARE_TIMES} compare takes",
                specs.len()
            ),
        )
        .at("times"));
    }
    let tile = if req.sheet {
        let rows = specs.len().min(SHEET_ROWS) as u32;
        let aspect = f64::from(size.0) / f64::from(size.1.max(1));
        picture::columns(rows * 3, aspect, IMAGE_DEFAULT_EDGE, 3)
    } else {
        None
    };
    if req.sheet && tile.is_none() {
        warnings.push(Warning::new(
            "sheet_left_out",
            "the video's shape leaves the sheet's tiles too small to label",
        ));
    }

    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by(|&a, &b| {
        let key = |s: &TimeSpec| match *s {
            TimeSpec::Time(t) => t.to_secs_f64(),
            TimeSpec::Frame(n) => n as f64 / info.frame_rate().map_or(1.0, |r| r.to_f64()),
        };
        key(&specs[a].1).total_cmp(&key(&specs[b].1))
    });
    let mut measured: Vec<Measured> = Vec::new();
    let mut longest = Duration::ZERO;
    let mut partial = false;
    for (k, i) in order.into_iter().enumerate() {
        let (arg, spec) = &specs[i];
        match cx.call.check() {
            Ok(()) if k > 0 && cx.call.remaining() < COMPARE_RESERVE + longest * 2 => {
                partial = true;
                break;
            }
            Ok(()) => {}
            Err(e) if e.code == codes::TIMED_OUT && !measured.is_empty() => {
                partial = true;
                break;
            }
            Err(e) => return Err(e),
        }
        let began = Instant::now();
        let a = file
            .seek(*spec, SeekMode::Exact, &mut warnings)
            .map_err(|f| at_time(cx, &f, arg, file.info()))?;
        let ours = flat(&mut file, &a, size).map_err(|f| failed(cx, &f, "file"))?;
        let (theirs, their_time) = match &mut against {
            Against::File { still, .. } => {
                // The time the first file showed, not the spec again: a
                // frame count ("40f") is the first file's.
                let b = still
                    .seek(TimeSpec::Time(a.shown.time), SeekMode::Exact, &mut warnings)
                    .map_err(|f| at_time(cx, &f, arg, still.info()))?;
                let img = flat(still, &b, size).map_err(|f| failed(cx, &f, "against_file"))?;
                (img, json!(b.shown))
            }
            Against::Project {
                loaded,
                frames,
                renderer,
                ..
            } => {
                let n = frames.at(TimeSpec::Time(a.shown.time)).ok_or_else(|| {
                    ToolError::new(
                        codes::OUT_OF_RANGE,
                        "is past the end of the project's render",
                    )
                    .at(arg.as_str())
                    .valid(format!("under {} s", seconds(frames.duration())))
                })?;
                let b = renderer
                    .seek(frames.source(n), &mut warnings)
                    .map_err(|f| at_time(cx, &f, arg, renderer.still().info()))?;
                let views = frames.views(&loaded.project, n, size);
                let img = renderer
                    .render(&b, &views, Depth::Sixteen)
                    .map_err(|f| failed(cx, &f, "against_project"))?;
                let shown = FrameTime {
                    time: frames.shown(n),
                    frame: Some(n),
                };
                (img, json!(shown))
            }
        };
        let (la, lb) = (luma(&ours), luma(&theirs));
        let plane = |d| Plane {
            width: size.0 as usize,
            height: size.1 as usize,
            data: d,
        };
        let psnr = psnr_plane(plane(&la), plane(&lb))
            .map_err(|e| ToolError::internal(format!("PSNR: {}", e.0)))?;
        let ssim = ssim_plane(plane(&la), plane(&lb))
            .map_err(|e| ToolError::internal(format!("SSIM: {}", e.0)))?;
        let thumbs = tile.map(|l| {
            let diff = |x, y| {
                let (p, q) = (unit(&ours, x, y), unit(&theirs, x, y));
                [0, 1, 2].map(|c| ((p[c] - q[c]).abs() * DIFF_GAIN).min(1.0))
            };
            [
                shrink(|x, y| unit(&ours, x, y), size, l.tile),
                shrink(|x, y| unit(&theirs, x, y), size, l.tile),
                shrink(diff, size, l.tile),
            ]
        });
        measured.push(Measured {
            index: i,
            time: a.shown,
            against: their_time,
            psnr,
            ssim,
            thumbs,
        });
        longest = longest.max(began.elapsed());
        cx.call
            .progress(measured.len() as f64, Some(specs.len() as f64), None);
    }
    drop(file);
    drop(opened);
    measured.sort_by_key(|m| m.index);

    let worst_psnr = measured.iter().min_by(|a, b| a.psnr.total_cmp(&b.psnr));
    let worst_ssim = measured.iter().min_by(|a, b| a.ssim.total_cmp(&b.ssim));
    let n = measured.len().max(1) as f64;
    // The mean PSNR is the mean error's, so that one identical time
    // (infinite PSNR) doesn't make the mean infinite.
    let mean_mse = measured
        .iter()
        .map(|m| 10f64.powf(-m.psnr / 10.0))
        .sum::<f64>()
        / n;
    let mean_psnr = if mean_mse > 0.0 {
        -10.0 * mean_mse.log10()
    } else {
        f64::INFINITY
    };
    let mean_ssim = measured.iter().map(|m| m.ssim).sum::<f64>() / n;
    let mut result = json!({
        "file": text::field(&req.file.to_string_lossy()),
        "against": match &against {
            Against::File { path, .. } => json!({"file": text::field(&path.to_string_lossy())}),
            Against::Project { path, frames, .. } => json!({
                "project": text::field(&path.to_string_lossy()),
                "from": frames.source(0),
            }),
        },
        "size": [size.0, size.1],
        "measure": "BT.709 luma as an encoder codes it: PSNR in dB (inf when identical), SSIM to 1",
        "times": measured.iter().map(|m| json!({
            "time": m.time,
            "against": m.against,
            "psnr": db(m.psnr),
            "ssim": round(m.ssim, 5),
        })).collect::<Vec<_>>(),
        "worst": {
            "psnr": worst_psnr.map(|m| json!({"time": m.time, "psnr": db(m.psnr)})),
            "ssim": worst_ssim.map(|m| json!({"time": m.time, "ssim": round(m.ssim, 5)})),
        },
        "mean": {"psnr": db(mean_psnr), "ssim": round(mean_ssim, 5)},
    });
    if partial {
        result["partial"] = json!(true);
        result["measured"] = json!(measured.len());
        result["next"] = json!("the call's time ran out: ask for the times not listed");
    }
    let mut images = Vec::new();
    if let Some(l) = tile {
        let mut worst: Vec<&Measured> = measured.iter().filter(|m| m.thumbs.is_some()).collect();
        worst.sort_by(|a, b| a.psnr.total_cmp(&b.psnr));
        worst.truncate(SHEET_ROWS);
        let rows = worst.len() as u32;
        let l = picture::columns(
            rows * 3,
            f64::from(size.0) / f64::from(size.1.max(1)),
            IMAGE_DEFAULT_EDGE,
            3,
        )
        .unwrap_or(l);
        let mut canvas = Canvas::new(l.size.0, l.size.1);
        let other = match &against {
            Against::File { .. } => "against_file",
            Against::Project { .. } => "the project's render",
        };
        for (r, m) in worst.iter().enumerate() {
            let Some(thumbs) = &m.thumbs else { continue };
            let when = look::when_label(&m.time);
            let lines = [
                [when.as_str(), "file"],
                [when.as_str(), other],
                [&db_label(m.psnr), "difference x4"],
            ];
            for (c, (img, label)) in thumbs.iter().zip(lines).enumerate() {
                let i = r * 3 + c;
                let (x, y) = l.origin(i);
                canvas.put(x, y + l.band, img);
                picture::caption(&mut canvas, (x, y), l.tile.0, l.scale, label);
            }
        }
        let (image, sent) = look::deliver(canvas, look::Format::Jpeg, None, cx)?;
        result["image"] = Value::Object(image);
        images = sent;
    }
    result["warnings"] = json!(look::warnings_json(&warnings));
    text::untrusted_under(&mut result, "reason");
    Ok((result, images))
}

/// `count` times spread evenly over `d`, each at the middle of its
/// part, so that none is at the very end.
fn spread(d: Time, count: u32) -> Result<Vec<Time>, ToolError> {
    let count = count.max(1);
    (0..count)
        .map(|i| {
            Rational::new(2 * i64::from(i) + 1, 2 * i64::from(count))
                .and_then(|f| d.as_rational().checked_mul(f))
                .map(Time::seconds)
                .map_err(|e| ToolError::internal(format!("spreading the times: {e}")))
        })
        .collect()
}

/// `path`, the argument `arg`, open for its frames drawn whole.
fn open_flat<'g>(
    cx: &Ctx,
    arg: &str,
    path: &Path,
    gpu: &'g Gpu,
    stop: &still::Stop,
    warnings: &mut Vec<Warning>,
) -> Result<(Opened, Still<'g>), ToolError> {
    let opened = cx.roots.open_file(arg, path)?;
    // The picture is compared as coded, whatever its metadata says.
    let opts = still::Options {
        equirect: true,
        ..still::Options::default()
    };
    let probed =
        still::probe(opened.source(), &opts, stop, warnings).map_err(|f| failed(cx, &f, arg))?;
    let s = Still::open(opened.source(), &probed, Some(gpu), &opts, stop, warnings)
        .map_err(|f| failed(cx, &f, arg))?;
    Ok((opened, s))
}

/// `d` drawn whole at `size`, 16 bits a channel.
fn flat(s: &mut Still<'_>, d: &Decoded, size: (u32, u32)) -> Result<Image, Failure> {
    let view = ViewParams::equirect(Ypr::from_degrees(0.0, 0.0, 0.0).to_quat());
    let look = still::Look {
        draw: Draw::Coded,
        filter: Filter::Isotropic,
        depth: Depth::Sixteen,
    };
    s.render(d, &[view], size, look)
}

/// A still's failure about `arg`, in look's words; a stop as the call
/// has it.
fn failed(cx: &Ctx, f: &Failure, arg: &str) -> ToolError {
    if f.code == codes::CANCELLED {
        return cx
            .call
            .check()
            .err()
            .unwrap_or_else(|| ToolError::new(codes::CANCELLED, "the call was stopped"));
    }
    match f.code {
        "no_gpu" | "adapter_not_found" | "out_of_gpu_memory" | "render_failed" | "device_lost" => {
            ToolError::new(codes::RENDER_FAILED, "the GPU failed to render a frame")
                .at(arg)
                .next("call again; doctor checks the GPU")
        }
        _ => look::file_failed(f).at(arg),
    }
}

/// A seek's failure at the time `arg`.
fn at_time(
    cx: &Ctx,
    f: &Failure,
    arg: &str,
    info: &spherecut_media::backend::VideoStreamInfo,
) -> ToolError {
    if f.code == codes::CANCELLED {
        return failed(cx, f, arg);
    }
    look::time_failed(f, arg, info)
}

/// Pixel `(x, y)` of `img`, its samples in unit scale.
fn unit(img: &Image, x: usize, y: usize) -> [f64; 3] {
    let i = 3 * (y * img.width as usize + x);
    [0, 1, 2].map(|c| match &img.pixels {
        Pixels::Rgb8(p) => f64::from(p[i + c]) / 255.0,
        Pixels::Rgb16(p) => f64::from(p[i + c]) / 65535.0,
        Pixels::LinearF32(p) => f64::from(p[i + c]),
    })
}

/// BT.709 luma of every pixel of `img` as an 8-bit encoder codes it, in
/// limited range over 255: the scale the export's parity tests measure
/// in (D52), so that compare's dB are theirs. From a file's RGB this is
/// its coded luma again, up to clamping.
fn luma(img: &Image) -> Vec<f64> {
    let (w, h) = (img.width as usize, img.height as usize);
    (0..w * h)
        .map(|i| {
            let p = unit(img, i % w, i / w);
            (16.0 + 219.0 * (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])) / 255.0
        })
        .collect()
}

/// A picture of `from` pixels, each `at(x, y)` in unit scale, averaged
/// down to `to`.
fn shrink(at: impl Fn(usize, usize) -> [f64; 3], from: (u32, u32), to: (u32, u32)) -> Image {
    let (fw, fh) = (from.0 as usize, from.1 as usize);
    let (tw, th) = (to.0.max(1) as usize, to.1.max(1) as usize);
    let mut out = Vec::with_capacity(tw * th * 3);
    for y in 0..th {
        let (y0, y1) = (
            y * fh / th,
            ((y + 1) * fh / th).max(y * fh / th + 1).min(fh),
        );
        for x in 0..tw {
            let (x0, x1) = (
                x * fw / tw,
                ((x + 1) * fw / tw).max(x * fw / tw + 1).min(fw),
            );
            let mut sum = [0.0; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = at(sx, sy);
                    for c in 0..3 {
                        sum[c] += p[c];
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as f64;
            out.extend(sum.map(|v| ((v / n).clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    Image {
        width: tw as u32,
        height: th as u32,
        pixels: Pixels::Rgb8(out),
    }
}

fn round(v: f64, places: i32) -> f64 {
    let k = 10f64.powi(places);
    (v * k).round() / k
}

/// A PSNR for the JSON: dB to two places, or `"inf"`.
fn db(v: f64) -> Value {
    if v.is_finite() {
        json!(round(v, 2))
    } else {
        json!("inf")
    }
}

fn db_label(v: f64) -> String {
    if v.is_finite() {
        format!("{:.2} dB", v)
    } else {
        "identical".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_spread_to_the_middles_of_equal_parts() {
        let d = Time::seconds(Rational::new(4, 1).unwrap());
        let t = spread(d, 4).unwrap();
        let s: Vec<f64> = t.iter().map(|t| t.to_secs_f64()).collect();
        assert_eq!(s, vec![0.5, 1.5, 2.5, 3.5]);
    }

    #[test]
    fn a_shrunk_picture_averages_its_blocks() {
        // 4x2 of grey levels 0 and 1, in 2x2 blocks: 0.0 and 1.0.
        let at = |x: usize, _y: usize| if x < 2 { [0.0; 3] } else { [1.0; 3] };
        let img = shrink(at, (4, 2), (2, 1));
        assert_eq!(img.pixels, Pixels::Rgb8(vec![0, 0, 0, 255, 255, 255]));
    }

    #[test]
    fn a_psnr_is_a_number_or_inf() {
        assert_eq!(db(48.1234), json!(48.12));
        assert_eq!(db(f64::INFINITY), json!("inf"));
    }
}
