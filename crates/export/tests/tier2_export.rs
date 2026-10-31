//! Tier 2: the export on the machine's GPU and its VA-API encoders (M2
//! plan section 11).
//!
//! - `export_vaapi_matches_software`: each format doctor's encode checks
//!   find working on VA-API (H.264, HEVC Main and Main 10, AV1, and AV1
//!   in 10 bits where AV1 works), exported through the VA-API encoder
//!   alone and through its software counterpart, the same project each
//!   time, both against the parity reference rendered on the same GPU.
//!   Each is held to PLAN's floor, 40 dB and 0.98, until odin's
//!   measurements give floors of their own; the measures go in the
//!   report's notes. The source decodes on VA-API: a decode that fell
//!   back fails the test.
//! - `export_360_8k_vaapi`: 2 s of the 8K perf clip to 360 at 7680×3840,
//!   in AV1 where doctor finds av1_vaapi working, else HEVC: V1 and V2
//!   read back, and A1's `check` passing it for `youtube_360`.
//! - `bench_export_4k30_vaapi`: a reframed 4K30 export (flat 3840×2160,
//!   H.264 at High, the flat default) of 10 s of the 5.7K perf clip, and
//!   of the first `.lrv` proxy among the samples where there is one:
//!   frames a second, each stage's time a frame, and the same in HEVC
//!   and as a draft. The gate is the default's 30 fps (PLAN's real-time
//!   criterion).
//! - `bench_export_vaapi_levels`: section 8.3's table, each of Mesa's
//!   quality levels in H.264 and HEVC (and AV1 where it works) at 4K
//!   from the 5.7K clip: its speed, its worst luma PSNR against the
//!   reference and its bitrate.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as
//! ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{
    Scratch, frames, grid, luma, measure, media_path, open, project, references_on, run, secs,
    with_encoder, worst,
};
use serde_json::{Value, json};
use spherecut_agent::Roots;
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_core::cancel::StopToken;
use spherecut_core::project::{Aspect, Codec, EncoderChoice, Orientation, Output, Project, Range};
use spherecut_core::time::Time;
use spherecut_export::encoder::quality_level::{
    BALANCED, HIGH_QUALITY, PRE_ENCODE, QUALITY, VALID, VBAQ,
};
use spherecut_export::{ExportJob, FramePlan, Report, StageTimes};
use spherecut_media::backend::{NextFrame, SeekMode, SeekOutcome, VideoFrame};
use spherecut_media::doctor::{self, CheckResult, Options};
use spherecut_media::hw::{self, GpuIds};
use spherecut_media::mp4;
use spherecut_media::spherical;
use spherecut_render::compare::{Plane, psnr_plane};
use spherecut_render::{AdapterChoice, Gpu, RenderError, YuvLayout};
use spherecut_sphere::orient::Ypr;
use spherecut_synth::media::{self as synth, PERF_5K7_8BIT, PERF_8K_8BIT};
use spherecut_testkit::{bench, tier2};

/// PLAN's least: the floor until odin's measurements set the encoders'
/// own.
const PLAN_FLOORS: (f64, f64) = (40.0, 0.98);

/// The GPU the tests render on: `SPHERECUT_ADAPTER`'s, else the best
/// there is; a skip when that is a CPU or there is none. One that is
/// there and won't give a device fails.
fn hardware_gpu() -> Gpu {
    tier2::require_hw_run();
    let choice = AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto);
    match Gpu::headless(&choice) {
        Ok(gpu) if gpu.caps.is_cpu => {
            tier2::skip(format!("{choice} is a CPU adapter, {}", gpu.caps.info.name))
        }
        Ok(gpu) => gpu,
        Err(e @ (RenderError::NoAdapter { .. } | RenderError::AdapterNotFound { .. })) => {
            tier2::skip(format!("no GPU ({choice}): {e}"))
        }
        Err(e) => panic!("{choice} won't open a device: {e}"),
    }
}

/// The render node the export takes for `gpu`: its PCI bus id's.
fn node_of(gpu: &Gpu) -> PathBuf {
    let info = &gpu.caps.info;
    let ids = GpuIds {
        pci_bus_id: info.device_pci_bus_id.clone(),
        vendor: info.vendor,
        device: info.device,
    };
    hw::render_node(Some(&ids), None).path
}

/// Doctor's encode checks on `gpu`'s node: the formats whose VA-API
/// encoder works, and the rest with why not.
fn vaapi_formats(gpu: &Gpu) -> (Vec<String>, Vec<String>) {
    let node = node_of(gpu);
    if !node.exists() {
        tier2::skip(format!("no DRM render node at {}", node.display()));
    }
    let report = doctor::run_with(
        &node,
        Options {
            query_packages: false,
        },
    );
    let (works, not): (Vec<_>, Vec<_>) = report
        .encode_checks
        .iter()
        .partition(|c| c.result == CheckResult::Works);
    let why = |c: &doctor::Check| {
        format!(
            "{} ({}): {:?}, {}",
            c.format,
            c.codec,
            c.result,
            c.reason.as_deref().unwrap_or("no reason")
        )
    };
    (
        works.iter().map(|c| c.format.clone()).collect(),
        not.iter().map(|c| why(c)).collect(),
    )
}

/// The trim from the start to `end`.
fn first(end: Time) -> Range {
    Range {
        start: Time::ZERO,
        end,
    }
}

/// `project` at `encoder`, exported on `gpu` to `out`.
fn export_on(gpu: &Gpu, project: &Project, encoder: EncoderChoice, out: &Path) -> Report {
    let mut p = project.clone();
    with_encoder(&mut p, encoder);
    run(ExportJob::new(p, out, gpu.clone()))
        .unwrap_or_else(|e| panic!("{} through {encoder:?}: {e}", out.display()))
}

/// The worst luma PSNR and SSIM of `out`, `project`'s export, against
/// the reference on `gpu`, every frame.
fn worst_against_reference(gpu: &Gpu, project: &Project, out: &Path, yuv: YuvLayout) -> (f64, f64) {
    let got = frames(out);
    let src = frames(&media_path(project));
    let all: Vec<u64> = (0..got.len() as u64).collect();
    let want = references_on(gpu, project, &src, yuv, &all);
    let m = measure(
        got.iter()
            .map(|g| g.as_ref())
            .zip(want.iter().map(Vec::as_slice)),
    );
    worst(&m)
}

/// One format of `export_vaapi_matches_software`.
struct Case {
    /// Doctor's name for it.
    format: &'static str,
    codec: Codec,
    /// The grid's name, codec and bits, as the parity tests make them.
    source: (&'static str, synth::Codec, u32),
    output: Output,
    end: Time,
    /// The planes the reference is in: 8 or 10 bits, whose luma is the
    /// encoder's.
    yuv: YuvLayout,
    encoders: (&'static str, &'static str),
}

fn cases() -> Vec<Case> {
    let flat = Output::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };
    let square = Output::Flat {
        aspect: Aspect::Square,
        short_side: 1080,
    };
    let sphere = Output::Equirect {
        width: 3840,
        orientation: Orientation::from_ypr(Ypr::from_degrees(30.0, 0.0, 0.0)).unwrap(),
    };
    let eight = ("parity-grid-h264-8", synth::Codec::H264, 8);
    let ten = ("parity-grid-hevc-10", synth::Codec::Hevc, 10);
    let case = |format, codec, source, output, end, yuv, encoders| Case {
        format,
        codec,
        source,
        output,
        end,
        yuv,
        encoders,
    };
    let (h264, hevc, av1) = (
        ("h264_vaapi", "libx264"),
        ("hevc_vaapi", "libx265"),
        ("av1_vaapi", "libsvtav1"),
    );
    let (two, half) = (Time::from_secs(2), secs(1, 2));
    vec![
        case(
            "H.264",
            Codec::H264,
            eight,
            flat,
            two,
            YuvLayout::I420,
            h264,
        ),
        case(
            "HEVC Main",
            Codec::Hevc,
            eight,
            flat,
            two,
            YuvLayout::I420,
            hevc,
        ),
        case(
            "HEVC Main 10",
            Codec::Hevc,
            ten,
            sphere,
            half,
            YuvLayout::I010,
            hevc,
        ),
        case("AV1", Codec::Av1, eight, square, half, YuvLayout::I420, av1),
        // Doctor checks AV1 in 8 bits; VCN 4 encodes 10 too.
        case("AV1", Codec::Av1, ten, square, half, YuvLayout::I010, av1),
    ]
}

#[test]
#[ignore = "tier2: needs a GPU with VA-API encode"]
fn export_vaapi_matches_software() {
    let gpu = hardware_gpu();
    let (works, not) = vaapi_formats(&gpu);
    bench::note(format!(
        "export parity on {}: VA-API encodes {works:?}; not {not:?}",
        gpu.caps.info.name
    ));
    if works.is_empty() {
        tier2::skip(format!("doctor finds no VA-API encoder working: {not:?}"));
    }
    let scratch = Scratch::new("tier2-parity");
    let mut ran = Vec::new();
    for c in cases()
        .iter()
        .filter(|c| works.iter().any(|w| w == c.format))
    {
        let (grid_name, codec, bits) = c.source;
        let src = grid(grid_name, codec, bits);
        let p = project(&src, first(c.end), c.output, c.codec, 0.0);
        let name = format!("{} from {bits}-bit", c.format);
        let mut measured = Vec::new();
        for (encoder, choice) in [
            (c.encoders.0, EncoderChoice::Vaapi),
            (c.encoders.1, EncoderChoice::Software),
        ] {
            let out = scratch.0.join(format!("{encoder}-{bits}.mp4"));
            let r = export_on(&gpu, &p, choice, &out);
            assert_eq!(r.encoder, encoder, "{name}");
            assert_eq!(r.fallback, None, "{name}: {encoder}");
            assert_eq!(r.ten_bit, c.yuv == YuvLayout::I010, "{name}: {encoder}");
            assert_eq!(
                (r.decode.path, &r.decode.fallback),
                ("vaapi", &None),
                "{name}: the source decodes on VA-API"
            );
            let (psnr, ssim) = worst_against_reference(&gpu, &p, &out, c.yuv);
            let line = format!(
                "{name} through {encoder}: worst luma PSNR {psnr:.2} dB, SSIM {ssim:.5} over {} \
                 frames, {:.1} Mb/s",
                r.frames,
                r.bytes as f64 * 8.0 / r.duration.to_secs_f64() / 1e6
            );
            eprintln!("{line}");
            bench::note(line);
            measured.push((encoder, psnr, ssim));
            std::fs::remove_file(&out).ok();
        }
        for (encoder, psnr, ssim) in measured {
            assert!(
                psnr >= PLAN_FLOORS.0 && ssim >= PLAN_FLOORS.1,
                "{name} through {encoder}: worst {psnr:.2} dB, SSIM {ssim:.5}, below PLAN's \
                 {PLAN_FLOORS:?}"
            );
        }
        ran.push(name);
    }
    eprintln!("ran {ran:?}");
}

/// A1's `check` of `file` for `target`: its result.
fn check(dir: &Path, file: &Path, target: &str) -> Value {
    let tools = Spherecut::new(Roots::new(&[dir.to_path_buf()], None).unwrap());
    let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
    let args = json!({"file": file, "target": target});
    tools.call("check", args, &cx).unwrap().result
}

#[test]
#[ignore = "tier2: needs a GPU with VA-API encode and the perf clips"]
fn export_360_8k_vaapi() {
    let clip = tier2::require_perf_clip(&format!("{PERF_8K_8BIT}.mp4"));
    let gpu = hardware_gpu();
    let (works, not) = vaapi_formats(&gpu);
    let (codec, encoder) = if works.iter().any(|w| w == "AV1") {
        (Codec::Av1, "av1_vaapi")
    } else if works.iter().any(|w| w == "HEVC Main") {
        (Codec::Hevc, "hevc_vaapi")
    } else {
        tier2::skip(format!(
            "doctor finds neither AV1 nor HEVC on VA-API: {not:?}"
        ));
    };
    let sphere = Output::Equirect {
        width: 7680,
        orientation: Orientation::IDENTITY,
    };
    let p = project(&clip, first(Time::from_secs(2)), sphere, codec, 0.0);
    let scratch = Scratch::new("tier2-360-8k");
    let out = scratch.0.join("sphere-8k.mp4");
    let r = export_on(&gpu, &p, EncoderChoice::Vaapi, &out);
    assert_eq!((r.encoder, r.size, r.frames), (encoder, (7680, 3840), 60));
    assert_eq!(r.fallback, None);
    eprintln!(
        "360 at 7680x3840 through {encoder}: {:.1} fps, {:.1} Mb/s, decoded on {}, {:?}",
        r.frames as f64 / r.elapsed.as_secs_f64(),
        r.bytes as f64 * 8.0 / r.duration.to_secs_f64() / 1e6,
        r.decode.path,
        r.stages
    );

    let tree = mp4::read_file(&out).unwrap();
    assert!(tree.damage().is_empty(), "{:?}", tree.damage());
    let kind = |t: &mp4::Mp4Box| mp4::handler_type(t).map(|h| h.0);
    let video = tree.tracks().find(|t| kind(t) == Some(*b"vide")).unwrap();
    let v = spherical::read_track(video);
    let v1 = v.v1.expect("V1");
    assert!(v1.spherical && v1.stitched, "{v1:?}");
    assert_eq!(v1.projection_type.as_deref(), Some("equirectangular"));
    let v2 = v.v2.expect("V2");
    assert_eq!(v2.stereo, Some(spherical::StereoMode::Mono));
    let checked = check(&scratch.0, &out, "youtube_360");
    assert_eq!(checked["pass"], true, "{checked:#}");
}

/// The 4K30 output the bench exports.
const UHD: Output = Output::Flat {
    aspect: Aspect::Wide,
    short_side: 2160,
};

/// `project` exported on `gpu` through VA-API alone, once to warm up
/// and [`bench::runs`] times: frames a second over the whole export, and
/// each stage's time a frame, recorded as `name`'s, and the median fps.
fn bench_export(gpu: &Gpu, name: &str, project: &Project, draft: bool, dir: &Path) -> f64 {
    let out = dir.join("bench.mp4");
    let mut fps = Vec::new();
    let mut stages: Vec<StageTimes> = Vec::new();
    let mut last = None;
    for run_n in 0..=bench::runs() {
        let mut p = project.clone();
        with_encoder(&mut p, EncoderChoice::Vaapi);
        let job = ExportJob {
            draft,
            overwrite: true,
            ..ExportJob::new(p, &out, gpu.clone())
        };
        let r = match run(job) {
            Ok(r) => r,
            Err(e) => {
                bench::not_measured(name, &e);
                panic!("{name}: {e}");
            }
        };
        assert_eq!(r.fallback, None, "{name}");
        if run_n > 0 {
            fps.push(r.frames as f64 / r.elapsed.as_secs_f64());
            stages.push(r.stages);
        }
        last = Some(r);
    }
    std::fs::remove_file(&out).ok();
    let r = last.unwrap();
    bench::note(format!(
        "{name}: {} frames at {}x{} through {}, decoded on {}{}",
        r.frames,
        r.size.0,
        r.size.1,
        r.encoder,
        r.decode.path,
        r.decode
            .fallback
            .as_ref()
            .map_or(String::new(), |f| format!(" (fell back: {})", f.reason)),
    ));
    bench::record_runs(name, &fps, "fps");
    let per_frame = |d: Duration| d.as_secs_f64() * 1e3 / r.frames as f64;
    for i in 0..parts(&r.stages).len() {
        let stage = parts(&r.stages)[i].0;
        let ms: Vec<f64> = stages.iter().map(|s| per_frame(parts(s)[i].1)).collect();
        bench::record_runs(&format!("{name}: {stage}"), &ms, "ms/frame");
    }
    bench::percentile(&fps, 50.0).unwrap()
}

/// Each stage's time, named.
fn parts(s: &StageTimes) -> [(&'static str, Duration); 5] {
    [
        ("decode", s.decode),
        ("render", s.render),
        ("readback", s.readback),
        ("encode", s.encode),
        ("finish", s.finish),
    ]
}

/// The first `.lrv` proxy under `dir`, by path.
fn first_lrv(dir: &Path) -> Option<PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lrv")) {
                found.push(p);
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

#[test]
#[ignore = "tier2: needs a GPU with VA-API encode and the perf clips"]
fn bench_export_4k30_vaapi() {
    let clip = tier2::require_perf_clip(&format!("{PERF_5K7_8BIT}.mp4"));
    let gpu = hardware_gpu();
    bench::note(format!("export on {}", gpu.caps.info.name));
    let scratch = Scratch::new("tier2-bench-4k30");
    let ten = first(Time::from_secs(10));
    let h264 = project(&clip, ten, UHD, Codec::H264, 0.0);
    let fps = bench_export(
        &gpu,
        "export 4k30 h264_vaapi high",
        &h264,
        false,
        &scratch.0,
    );
    let hevc = project(&clip, ten, UHD, Codec::Hevc, 0.0);
    bench_export(
        &gpu,
        "export 4k30 hevc_vaapi high",
        &hevc,
        false,
        &scratch.0,
    );
    bench_export(
        &gpu,
        "export 4k30 h264_vaapi draft",
        &h264,
        true,
        &scratch.0,
    );
    match tier2::samples_dir().and_then(|d| first_lrv(&d)) {
        Some(lrv) => {
            let duration = open(&lrv).info().duration.unwrap();
            let end = std::cmp::min(duration, Time::from_secs(10));
            let p = project(&lrv, first(end), UHD, Codec::H264, 0.0);
            bench::note(format!("the LRV is {}", lrv.display()));
            bench_export(
                &gpu,
                "export 4k30 from lrv h264_vaapi high",
                &p,
                false,
                &scratch.0,
            );
        }
        None => bench::not_measured(
            "export 4k30 from lrv h264_vaapi high",
            "no .lrv proxy among the samples",
        ),
    }
    assert!(
        fps >= 30.0,
        "a 4K30 export of the 5.7K clip in H.264 at High runs at {fps:.1} fps, under real time"
    );
}

/// Frames `which` of `path`, decoded in software from the start.
fn sampled(path: &Path, which: &[u64]) -> Vec<Arc<VideoFrame>> {
    let mut dec = open(path);
    let stop = StopToken::never();
    let SeekOutcome::Frame { frame, .. } = dec.seek(Time::ZERO, SeekMode::Exact, &stop).unwrap()
    else {
        panic!("seek stopped");
    };
    let mut out = Vec::new();
    let mut next = Some(frame);
    let mut n = 0;
    while let Some(f) = next.take() {
        if which.contains(&n) {
            out.push(f);
        }
        if out.len() == which.len() {
            break;
        }
        n += 1;
        if let NextFrame::Frame(f) = dec.next_frame(&stop).unwrap() {
            next = Some(f);
        }
    }
    assert_eq!(out.len(), which.len(), "{} ends early", path.display());
    out
}

#[test]
#[ignore = "tier2: needs a GPU with VA-API encode and the perf clips"]
fn bench_export_vaapi_levels() {
    let clip = tier2::require_perf_clip(&format!("{PERF_5K7_8BIT}.mp4"));
    let gpu = hardware_gpu();
    let (works, not) = vaapi_formats(&gpu);
    bench::note(format!(
        "VA-API quality levels on {}: not {not:?}",
        gpu.caps.info.name
    ));
    let levels: [(&str, u32); 7] = [
        ("speed", VALID),
        ("balanced", VALID | BALANCED),
        ("balanced vbaq", VALID | BALANCED | VBAQ),
        ("quality", VALID | QUALITY),
        ("quality pre-encode", VALID | QUALITY | PRE_ENCODE),
        (
            "quality pre-encode vbaq",
            VALID | QUALITY | PRE_ENCODE | VBAQ,
        ),
        (
            "high quality pre-encode vbaq",
            VALID | HIGH_QUALITY | PRE_ENCODE | VBAQ,
        ),
    ];
    let scratch = Scratch::new("tier2-levels");
    let out = scratch.0.join("level.mp4");
    let trim = first(Time::from_secs(2));
    let which: Vec<u64> = (0..60).step_by(10).collect();
    // The reference is the same for every codec and level.
    let base = project(&clip, trim, UHD, Codec::H264, 0.0);
    let plan = FramePlan::new(base.the_clip().source, base.the_sequence().rate).unwrap();
    let src = sampled(&clip, &which);
    for (f, &n) in src.iter().zip(&which) {
        assert_eq!(f.time, plan.source_time(n).unwrap(), "source frame {n}");
    }
    let want = references_on(&gpu, &base, &src, YuvLayout::I420, &which);
    drop(src);
    for (format, codec, encoder) in [
        ("H.264", Codec::H264, "h264_vaapi"),
        ("HEVC Main", Codec::Hevc, "hevc_vaapi"),
        ("AV1", Codec::Av1, "av1_vaapi"),
    ] {
        if !works.iter().any(|w| w == format) {
            bench::not_measured(
                &format!("export levels {encoder}"),
                "doctor finds it not working",
            );
            continue;
        }
        let mut p = project(&clip, trim, UHD, codec, 0.0);
        with_encoder(&mut p, EncoderChoice::Vaapi);
        for (level_name, level) in levels {
            let name = format!("export levels {encoder} {level_name} ({level})");
            let job = ExportJob {
                vaapi_level: Some(level),
                overwrite: true,
                ..ExportJob::new(p.clone(), &out, gpu.clone())
            };
            let r = match run(job) {
                Ok(r) => r,
                Err(e) => {
                    bench::not_measured(&name, &e);
                    continue;
                }
            };
            let got = sampled(&out, &which);
            let psnr: Vec<f64> = got
                .iter()
                .zip(&want)
                .map(|(g, w)| {
                    let (width, height, l) = luma(g.as_ref());
                    let plane = |data| Plane {
                        width,
                        height,
                        data,
                    };
                    psnr_plane(plane(l.as_slice()), plane(w.as_slice())).unwrap()
                })
                .collect();
            let worst = psnr.iter().copied().fold(f64::INFINITY, f64::min);
            bench::record(
                &format!("{name}: speed"),
                r.frames as f64 / r.elapsed.as_secs_f64(),
                "fps",
            );
            bench::record(&format!("{name}: worst luma PSNR"), worst, "dB");
            bench::record(
                &format!("{name}: bitrate"),
                r.bytes as f64 * 8.0 / r.duration.to_secs_f64() / 1e6,
                "Mb/s",
            );
        }
    }
    std::fs::remove_file(&out).ok();
}
