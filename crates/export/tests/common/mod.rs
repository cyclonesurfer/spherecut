//! What the export's tests share: the lavapipe device, the synthetic
//! clips, projects of them, decoding an export again, and the parity
//! reference with its measures.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use spherecut_core::cancel::StopToken;
use spherecut_core::project::file::head_hash;
use spherecut_core::project::{
    AnyKeys, AudioChoice, Codec, Ease, Edit, EncoderChoice, ExportSettings, Fingerprint,
    InitialView, Key, KeyId, Media, MediaId, MediaVideo, Orient, Orientation, Output, Project,
    Quality, Range,
};
use spherecut_core::time::{Rational, Time};
use spherecut_core::video::VideoPlanes;
use spherecut_export::{Export, ExportJob, FramePlan, Report};
use spherecut_media::backend::{
    FfmpegBackend, MediaBackend, NextFrame, SeekMode, SeekOutcome, VideoDecode, VideoFrame,
    VideoRequest,
};
use spherecut_render::output::frame_views;
use spherecut_render::{
    AdapterChoice, Filter, Gpu, Placement, StillOptions, StillRenderer, YuvImage, YuvLayout,
};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::Projection;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self as synth, Audio, Spec};
use spherecut_testkit::compare::{Plane, psnr_plane, ssim_plane};

pub fn gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(|| {
        Gpu::headless(&AdapterChoice::Cpu)
            .expect("lavapipe, Mesa's Vulkan CPU driver, is needed for the export tests")
    })
}

/// Where the tests' clips are made, once.
pub fn media_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("export-media")
}

pub fn clip(spec: &Spec) -> PathBuf {
    let path = media_dir().join(spec.file_name());
    if path.exists() {
        return path;
    }
    synth::write(spec, &media_dir()).expect("writing a test clip")
}

/// 2 s of the grid at 30 fps, 2048×1024 equirect: the moving marker on
/// the equator and the frame counter at −20°.
pub fn grid(name: &str, codec: synth::Codec, bits: u32) -> PathBuf {
    clip(&Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 2048,
        height: 1024,
        frames: 60,
        codec,
        bits,
        audio: Audio::None,
        spherical: true,
        ..Spec::default()
    })
}

/// A scratch folder for one test's output.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("export-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub fn open(path: &Path) -> Box<dyn VideoDecode> {
    FfmpegBackend::new()
        .open_video(path, &VideoRequest::default())
        .unwrap()
}

pub fn secs(num: i64, den: i64) -> Time {
    Time::seconds(Rational::new(num, den).unwrap())
}

pub fn orient(yaw: f64, pitch: f64) -> Orient {
    Orient::View(Orientation::from_ypr(Ypr::from_degrees(yaw, pitch, 0.0)).unwrap())
}

/// A project of `path`'s clip, trimmed to `trim`, exporting `output` in
/// `codec`, the view panning from yaw `yaw0` at 10° a second over the
/// whole clip, 15° down.
pub fn project(path: &Path, trim: Range, output: Output, codec: Codec, yaw0: f64) -> Project {
    let dec = open(path);
    let info = dec.info();
    let media = Media {
        id: MediaId::NEW,
        path: path.to_path_buf(),
        fingerprint: Fingerprint {
            bytes: std::fs::metadata(path).unwrap().len(),
            head: head_hash(path).unwrap(),
        },
        video: MediaVideo {
            stream: info.index as u32,
            width: info.size.0,
            height: info.size.1,
            rate: info.frame_rate().unwrap(),
            duration: info.duration.unwrap(),
        },
    };
    let view = InitialView {
        orientation: Orientation::from_ypr(Ypr::from_degrees(yaw0, -15.0, 0.0)).unwrap(),
        fov_deg: 90.0,
        projection: Projection::Rectilinear,
    };
    let duration = media.video.duration;
    let mut p = Project::untitled(media, view).unwrap();
    let (sequence, clip) = (p.the_sequence().id, p.the_clip().id);
    let key = |at, yaw| Key {
        id: KeyId::NEW,
        at,
        value: orient(yaw, -15.0),
        ease: Ease::LINEAR,
        spin: 0,
    };
    p.apply(&[
        Edit::SetKeys {
            clip,
            keys: AnyKeys::Orientation(vec![
                key(Time::ZERO, yaw0),
                key(duration, yaw0 + 10.0 * duration.to_secs_f64()),
            ]),
        },
        Edit::SetTrim {
            clip,
            start: Some(trim.start),
            end: Some(trim.end),
        },
        Edit::SetOutput { sequence, output },
        Edit::SetExport {
            sequence,
            export: ExportSettings {
                codec,
                quality: Quality::High,
                encoder: EncoderChoice::Software,
                audio: AudioChoice::None,
                motion_blur: false,
            },
        },
    ])
    .unwrap();
    p
}

/// `project` exporting its audio as `audio`.
pub fn with_audio(project: &mut Project, audio: AudioChoice) {
    let sequence = project.the_sequence();
    let export = ExportSettings {
        audio,
        ..sequence.export
    };
    let id = sequence.id;
    project
        .apply(&[Edit::SetExport {
            sequence: id,
            export,
        }])
        .unwrap();
}

pub fn export(project: &Project, out: &Path) -> Report {
    let job = ExportJob::new(project.clone(), out, gpu().clone());
    let mut e = Export::start(job).unwrap();
    let report = e.wait(Duration::from_secs(1800)).expect("the export ends");
    let report = report.unwrap();
    assert_eq!(e.progress().frames_done, report.frames);
    report
}

/// Every frame of `path` from the start, in order.
pub fn frames(path: &Path) -> Vec<Arc<VideoFrame>> {
    let mut dec = open(path);
    let stop = StopToken::never();
    let SeekOutcome::Frame { frame, .. } = dec.seek(Time::ZERO, SeekMode::Exact, &stop).unwrap()
    else {
        panic!("seek stopped");
    };
    let mut out = vec![frame];
    while let NextFrame::Frame(f) = dec.next_frame(&stop).unwrap() {
        out.push(f);
    }
    out
}

pub fn media_path(project: &Project) -> PathBuf {
    let clip = project.the_clip();
    project.media_by_id(clip.media).unwrap().path.clone()
}

/// The luma plane in unit scale.
pub fn luma(frame: &dyn VideoPlanes) -> (usize, usize, Vec<f64>) {
    let p = frame.plane(0).unwrap();
    let (w, h) = (p.width as usize, p.height as usize);
    let deep = frame.layout().bit_depth > 8;
    let max = f64::from((1u32 << frame.layout().bit_depth) - 1);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = &p.bytes[y * p.stride..];
        for x in 0..w {
            let v = if deep {
                u16::from_le_bytes([row[2 * x], row[2 * x + 1]])
            } else {
                u16::from(row[x])
            };
            out.push(f64::from(v) / max);
        }
    }
    (w, h, out)
}

pub fn reference_luma(image: &YuvImage) -> Vec<f64> {
    let (w, h) = image.shape.size;
    let max = f64::from((1u32 << image.shape.layout.bits()) - 1);
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| f64::from(image.sample(0, x, y)) / max)
        .collect()
}

/// The source frame shown at `t`: the last at or before it, or the
/// first.
pub fn held(source: &[Arc<VideoFrame>], t: Time) -> &VideoFrame {
    source
        .iter()
        .rev()
        .find(|f| f.time <= t)
        .unwrap_or(&source[0])
}

/// The parity reference of frames `which` of `project`'s export: the
/// views of the frame's time through `StillRenderer`, from the source
/// frame shown then, in the export's planes.
pub fn references(
    project: &Project,
    source: &[Arc<VideoFrame>],
    yuv: YuvLayout,
    which: &[u64],
) -> Vec<Vec<f64>> {
    let gpu = gpu();
    let sequence = project.the_sequence();
    let clip = project.the_clip();
    let plan = FramePlan::new(clip.source, sequence.rate).unwrap();
    let opts = StillOptions {
        filter: if gpu.caps.anisotropy {
            Filter::Anisotropic
        } else {
            Filter::Isotropic
        },
        placement: Placement::FULL,
        ..StillOptions::default()
    };
    let frame_s = plan.rate().frame_duration().to_secs_f64();
    let mut renderer = StillRenderer::new(gpu);
    which
        .iter()
        .map(|&n| {
            let t = plan.source_time(n).unwrap();
            let src = held(source, t);
            let views = frame_views(
                &clip.view,
                project.targets(),
                &sequence.output,
                t.to_secs_f64(),
                frame_s,
                false,
            );
            let image = renderer
                .render_yuv(gpu, src, &views.views, views.size, &opts, yuv)
                .unwrap();
            reference_luma(&image)
        })
        .collect()
}

/// PSNR and SSIM of each decoded frame's luma against its reference.
pub fn measure<'a>(pairs: impl Iterator<Item = (&'a VideoFrame, &'a [f64])>) -> Vec<(f64, f64)> {
    pairs
        .map(|(g, w)| {
            let (width, height, l) = luma(g);
            let a = Plane {
                width,
                height,
                data: &l,
            };
            let b = Plane {
                width,
                height,
                data: w,
            };
            (psnr_plane(a, b).unwrap(), ssim_plane(a, b).unwrap())
        })
        .collect()
}

pub fn worst(m: &[(f64, f64)]) -> (f64, f64) {
    m.iter().fold((f64::INFINITY, 1.0f64), |(p, s), &(q, t)| {
        (p.min(q), s.min(t))
    })
}

pub fn best(m: &[(f64, f64)]) -> (f64, f64) {
    m.iter()
        .fold((0.0f64, 0.0f64), |(p, s), &(q, t)| (p.max(q), s.max(t)))
}
