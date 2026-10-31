//! The export's three threads (M2 plan section 8.2): decode, render, and
//! encode and mux, joined by bounded channels and stopped together by
//! one [`StopSource`].
//!
//! The encode stage runs on the job's own thread and owns the writer;
//! decode and render get threads of their own. Each stage ends on its
//! own error, on the stop, or when the stage after it has gone (its
//! channel closed), and [`run`] reports the first stage that failed for
//! a reason of its own.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use spherecut_core::brand::APP_NAME;
use spherecut_core::cancel::{StopSource, StopToken};
use spherecut_core::project::{Output, Target, ViewAnim};
use spherecut_core::time::Time;
use spherecut_media::backend::{
    DecodeMode, DecoderEvent, Exactness, FfmpegBackend, MediaBackend, NextFrame, SeekMode,
    SeekOutcome, VideoDecode, VideoFrame, VideoRequest, VideoStreamInfo,
};
use spherecut_media::encode::{EncodeError, Mp4Writer, StridedPlanes, VideoSettings};
use spherecut_media::inject::{EntryEdit, TrackKind};
use spherecut_media::spherical::{StereoMode, write};
use spherecut_render::output::frame_views;
use spherecut_render::{
    Filter, Gpu, Placement, Stereo, StillOptions, StillRenderer, YuvImage, YuvLayout,
};

use crate::encoder::EncoderSetup;
use crate::error::{ExportError, Stage};
use crate::job::{ExportJob, Report, Shared, panic_text};
use crate::plan::FramePlan;
use crate::preset::Preset;

/// Decoded frames waiting for the GPU: the decoder runs a frame or two
/// ahead.
const DECODED_AHEAD: usize = 2;

/// Rendered frames waiting for the encoder: with the one being rendered
/// and the one being encoded, section 8.2's four readback buffers.
const RENDERED_AHEAD: usize = 2;

/// How often the encode stage looks at the stop while it waits.
const POLL: Duration = Duration::from_millis(50);

/// The source frame shown at output frame `n`.
struct Held {
    n: u64,
    frame: Arc<VideoFrame>,
}

/// Output frame `n`, in the encoder's planes.
struct Rendered {
    n: u64,
    image: YuvImage,
    capped: bool,
}

/// How a stage ended.
enum Ended {
    Done,
    /// The export was stopped.
    Stopped,
    /// The stage after this one went away, having failed or stopped.
    Downstream,
    Failed(ExportError),
}

/// What the render stage needs of the project.
struct Look {
    anim: ViewAnim,
    targets: Vec<Target>,
    output: Output,
    blur: bool,
    opts: StillOptions,
    yuv: YuvLayout,
}

/// The whole export, on the job's thread.
pub(crate) fn run(
    job: &ExportJob,
    preset: Preset,
    plan: FramePlan,
    shared: &Shared,
    stop: &StopSource,
) -> Result<Report, ExportError> {
    let sequence = job.project.the_sequence();
    let clip = job.project.the_clip();
    let media = job
        .project
        .media_by_id(clip.media)
        .ok_or_else(|| ExportError::Invalid("the clip's media isn't in the project".into()))?;

    // Software decoding for now; section 8.2's choice of path comes with
    // the hardware encoders (M2.5).
    let backend = FfmpegBackend::new();
    let req = VideoRequest {
        stream: Some(media.video.stream as usize),
        decode: DecodeMode::Software,
        interrupt: stop.shutdown_flag(),
        ..VideoRequest::default()
    };
    let dec = backend.open_video(&media.path, &req).map_err(|e| {
        if stop.is_shut_down() {
            return ExportError::Cancelled;
        }
        ExportError::failed(
            Stage::Decode,
            None,
            format!("opening {}: {e}", media.path.display()),
        )
    })?;
    let info = dec.info().clone();
    let (placement, warnings) = placement(&info);
    let ten_bit = info.layout.bit_depth > 8 && !job.eight_bit;
    let s = &job.settings;
    let setup = EncoderSetup::software(s.codec, s.quality, preset, plan.rate(), ten_bit);
    let size = preset.size();

    let look = Look {
        anim: clip.view.clone(),
        targets: job.project.targets().to_vec(),
        output: sequence.output,
        blur: s.motion_blur,
        opts: StillOptions {
            filter: filter(&job.gpu),
            placement,
            ..StillOptions::default()
        },
        yuv: setup.yuv,
    };
    let token = stop.current();
    let (held_tx, held_rx) = mpsc::sync_channel(DECODED_AHEAD);
    let (yuv_tx, yuv_rx) = mpsc::sync_channel(RENDERED_AHEAD);
    let decoding = spawn("export-decode", {
        let token = token.clone();
        move || decode(dec, plan, held_tx, token)
    })?;
    let rendering = spawn("export-render", {
        let gpu = job.gpu.clone();
        move || render(gpu, look, plan, held_rx, yuv_tx, token)
    })?;
    let encoded = encode(job, &setup, size, plan, yuv_rx, shared, stop);
    let decoded = join(decoding, Stage::Decode);
    let rendered = join(rendering, Stage::Render);
    if stop.is_shut_down() {
        return Err(ExportError::Cancelled);
    }
    for ended in [decoded, rendered] {
        if let Ended::Failed(e) = ended {
            return Err(e);
        }
    }
    let (writer, blur_capped) = encoded?;

    let edits = match sequence.output {
        Output::Equirect { .. } => vec![EntryEdit {
            track: TrackKind::Video,
            boxes: [
                write::st3d(StereoMode::Mono),
                write::sv3d_equirect(APP_NAME),
            ]
            .concat(),
        }],
        Output::Flat { .. } => Vec::new(),
    };
    let tmp = writer.finish_temp(&edits).map_err(|e| match e {
        EncodeError::Inject(e) => ExportError::failed(Stage::Metadata, None, e.to_string()),
        e => file_or(Stage::Mux, None, e),
    })?;
    if stop.is_shut_down() {
        return Err(ExportError::Cancelled);
    }
    let persisted = if job.overwrite {
        tmp.persist_replace()
    } else {
        tmp.persist_new()
    };
    persisted.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => ExportError::OutputExists(job.output.clone()),
        _ => ExportError::failed(
            Stage::File,
            None,
            format!("putting {} in place: {e}", job.output.display()),
        ),
    })?;
    let bytes = std::fs::metadata(&job.output).map_or(0, |m| m.len());
    Ok(Report {
        output: job.output.clone(),
        frames: plan.count(),
        duration: plan.duration(),
        size,
        rate: plan.rate(),
        codec: s.codec,
        encoder: setup.name,
        ten_bit,
        bytes,
        blur_capped,
        warnings,
        elapsed: shared.elapsed(),
    })
}

fn spawn(
    name: &str,
    f: impl FnOnce() -> Ended + Send + 'static,
) -> Result<JoinHandle<Ended>, ExportError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| ExportError::Invalid(format!("starting the export's {name} thread: {e}")))
}

/// How a stage's thread ended; a panic fails `stage`.
fn join(t: JoinHandle<Ended>, stage: Stage) -> Ended {
    t.join()
        .unwrap_or_else(|p| Ended::Failed(ExportError::failed(stage, None, panic_text(&*p))))
}

/// The export samples as the viewer does by default, so it looks as it
/// did in the window (D49): anisotropic where the GPU filters so.
fn filter(gpu: &Gpu) -> Filter {
    if gpu.caps.anisotropy {
        Filter::Anisotropic
    } else {
        Filter::Isotropic
    }
}

/// Where the stream's picture sits on the sphere, as playback and
/// `spherecut frame` place it, and what the export does about the
/// mapping that the project doesn't say.
fn placement(info: &VideoStreamInfo) -> (Placement, Vec<String>) {
    let m = &info.mapping;
    let mut warnings = Vec::new();
    let stereo = match m.stereo {
        StereoMode::Mono => Stereo::Mono,
        StereoMode::TopBottom => {
            warnings.push("stereo, top-bottom: exporting the left eye (the top half)".into());
            Stereo::TopBottom
        }
        StereoMode::LeftRight => {
            warnings.push("stereo, left-right: exporting the left eye (the left half)".into());
            Stereo::LeftRight
        }
        other => {
            warnings.push(format!(
                "stereo layout {other:?} is unknown: exporting the whole picture, as mono"
            ));
            Stereo::Mono
        }
    };
    let [u0, v0, u1, v1] = m.cover;
    if m.cover != [0.0, 0.0, 1.0, 1.0] {
        warnings.push("the picture covers part of the sphere: the rest is black".into());
    }
    if m.pose_present {
        warnings.push("the file's pose is not applied".into());
    }
    let placement = Placement {
        left: u0,
        top: v0,
        right: 1.0 - u1,
        bottom: 1.0 - v1,
        stereo,
    };
    (placement, warnings)
}

/// The decode stage: the source frame shown at each output frame's time,
/// the last with a presentation time at or before it (frame hold), in
/// order. A frame the decoder can't give fails the export with its time.
fn decode(
    mut dec: Box<dyn VideoDecode>,
    plan: FramePlan,
    tx: SyncSender<Held>,
    token: StopToken,
) -> Ended {
    let fail = |n: u64, message: String| {
        Ended::Failed(ExportError::failed(
            Stage::Decode,
            plan.sequence_time(n).ok(),
            message,
        ))
    };
    // The plan checked that every frame's time fits.
    let Ok(start) = plan.source_time(0) else {
        return fail(0, "the trim's start doesn't fit".into());
    };
    let mut cur = match dec.seek(start, SeekMode::Exact, &token) {
        Ok(SeekOutcome::Stopped) => return Ended::Stopped,
        Ok(SeekOutcome::Frame {
            frame, exactness, ..
        }) => match exactness {
            Exactness::Exact | Exactness::BeforeFirst => frame,
            Exactness::AfterLast => {
                return fail(0, "the trim starts after the clip's last frame".into());
            }
            Exactness::Truncated { .. } => {
                return fail(0, "the file is damaged before the trim's start".into());
            }
            Exactness::KeyframeOnly => return fail(0, "an exact seek landed on a keyframe".into()),
        },
        Err(e) if token.is_stopped() => {
            drop(e);
            return Ended::Stopped;
        }
        Err(e) => return fail(0, e.to_string()),
    };
    let mut ahead: Option<Arc<VideoFrame>> = None;
    let mut ended = false;
    for n in 0..plan.count() {
        if token.is_stopped() {
            return Ended::Stopped;
        }
        let Ok(t) = plan.source_time(n) else {
            return fail(n, "the frame's time doesn't fit".into());
        };
        loop {
            let next = match ahead.take() {
                Some(f) => f,
                None if ended => break,
                None => match dec.next_frame(&token) {
                    Ok(NextFrame::Frame(f)) => f,
                    Ok(NextFrame::End) => {
                        ended = true;
                        break;
                    }
                    Ok(NextFrame::Stopped) => return Ended::Stopped,
                    Err(_) if token.is_stopped() => return Ended::Stopped,
                    Err(e) => return fail(n, e.to_string()),
                },
            };
            if next.time <= t {
                cur = next;
            } else {
                ahead = Some(next);
                break;
            }
        }
        if let Some(why) = damage(dec.take_events()) {
            if token.is_stopped() {
                return Ended::Stopped;
            }
            return fail(n, why);
        }
        if cur.concealed {
            return fail(
                n,
                "the source frame is damaged (the decoder concealed errors)".into(),
            );
        }
        if ended
            && let Some(d) = cur.duration
            && cur.time.checked_add(d).is_ok_and(|end| end <= t)
        {
            return fail(
                n,
                format!(
                    "the clip's last frame ends at {:.3} s, before this frame",
                    cur.time.checked_add(d).map_or(0.0, Time::to_secs_f64)
                ),
            );
        }
        let held = Held {
            n,
            frame: cur.clone(),
        };
        if tx.send(held).is_err() {
            return Ended::Downstream;
        }
    }
    Ended::Done
}

/// The first damage among a decoder's events, said.
fn damage(events: Vec<DecoderEvent>) -> Option<String> {
    events.into_iter().find_map(|e| match e {
        DecoderEvent::Damaged { reason, cause, .. } => {
            Some(format!("the file is damaged ({}): {reason}", cause.code()))
        }
        _ => None,
    })
}

/// The render stage: each held frame through the frame's views, into
/// the encoder's planes.
fn render(
    gpu: Gpu,
    look: Look,
    plan: FramePlan,
    rx: Receiver<Held>,
    tx: SyncSender<Rendered>,
    token: StopToken,
) -> Ended {
    let mut renderer = StillRenderer::new(&gpu);
    let frame_s = plan.rate().frame_duration().to_secs_f64();
    for held in rx {
        if token.is_stopped() {
            return Ended::Stopped;
        }
        let t = plan.source_time(held.n).map_or(f64::NAN, Time::to_secs_f64);
        let views = frame_views(
            &look.anim,
            &look.targets,
            &look.output,
            t,
            frame_s,
            look.blur,
        );
        let image = match renderer.render_yuv(
            &gpu,
            held.frame.as_ref(),
            &views.views,
            views.size,
            &look.opts,
            look.yuv,
        ) {
            Ok(i) => i,
            Err(e) => {
                return Ended::Failed(ExportError::failed(
                    Stage::Render,
                    plan.sequence_time(held.n).ok(),
                    e.to_string(),
                ));
            }
        };
        let rendered = Rendered {
            n: held.n,
            image,
            capped: views.capped,
        };
        if tx.send(rendered).is_err() {
            return Ended::Downstream;
        }
    }
    Ended::Done
}

/// The encode stage: every frame into the writer, in order. Returns the
/// writer, all frames pushed, and the count of frames whose blur was
/// capped.
fn encode(
    job: &ExportJob,
    setup: &EncoderSetup,
    (width, height): (u32, u32),
    plan: FramePlan,
    rx: Receiver<Rendered>,
    shared: &Shared,
    stop: &StopSource,
) -> Result<(Mp4Writer, u64), ExportError> {
    let settings = VideoSettings {
        encoder: setup.name.into(),
        width,
        height,
        rate: plan.rate(),
        layout: setup.layout,
        options: setup.options.clone(),
        color: setup.color,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let mut writer = Mp4Writer::create(&job.output, &settings, None)
        .map_err(|e| writer_error(&job.output, e))?;
    let mut capped = 0;
    let mut next = 0;
    while next < plan.count() {
        if stop.is_shut_down() {
            return Err(ExportError::Cancelled);
        }
        let r = match rx.recv_timeout(POLL) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => continue,
            // An earlier stage ended; `run` says why.
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let at = plan.sequence_time(r.n).ok();
        if r.n != next {
            return Err(ExportError::failed(
                Stage::Encode,
                at,
                format!("frame {} arrived where frame {next} should", r.n),
            ));
        }
        let shape = &r.image.shape;
        let p = |i: usize| {
            (
                shape.planes[i].offset as usize,
                shape.planes[i].stride as usize,
            )
        };
        let planes = StridedPlanes {
            data: &r.image.data,
            planes: [p(0), p(1), p(2)],
        };
        writer
            .push_video_strided(&planes)
            .map_err(|e| file_or(Stage::Encode, at, e))?;
        capped += u64::from(r.capped);
        next += 1;
        shared.muxed(next);
    }
    if next < plan.count() {
        // `run` reports the stage that ended early; this is only what is
        // left if none did.
        return Err(ExportError::failed(
            Stage::Encode,
            plan.sequence_time(next).ok(),
            "the frames stopped coming",
        ));
    }
    Ok((writer, capped))
}

/// A writer that couldn't start: no encoder, or no file.
fn writer_error(output: &Path, e: EncodeError) -> ExportError {
    match e {
        EncodeError::NoEncoder(name) => {
            ExportError::NoEncoder(format!("this FFmpeg has no {name}"))
        }
        EncodeError::Io { error, .. } => ExportError::failed(
            Stage::File,
            None,
            format!("creating a file beside {}: {error}", output.display()),
        ),
        e => ExportError::failed(Stage::Encode, None, e.to_string()),
    }
}

/// `e` as a failure of `stage`, or of the file when it is the disk's.
fn file_or(stage: Stage, at: Option<Time>, e: EncodeError) -> ExportError {
    match e {
        EncodeError::Io { .. } => ExportError::failed(Stage::File, at, e.to_string()),
        e => ExportError::failed(stage, at, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use spherecut_core::project::Range;
    use spherecut_core::time::{FrameRate, Rational};
    use spherecut_synth::media::{self as synth, Spec, VFR_TIMESCALE, vfr_ticks};

    use super::*;

    /// A test's own directory in the system's temp dir, removed when
    /// dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("spherecut-export-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn secs(num: i64, den: i64) -> Time {
        Time::seconds(Rational::new(num, den).unwrap())
    }

    fn open(path: &Path) -> Box<dyn VideoDecode> {
        let req = VideoRequest {
            decode: DecodeMode::Software,
            ..VideoRequest::default()
        };
        FfmpegBackend::new().open_video(path, &req).unwrap()
    }

    /// Every frame's time, in order.
    fn times(path: &Path) -> Vec<Time> {
        let mut dec = open(path);
        let stop = StopToken::never();
        let Ok(SeekOutcome::Frame { frame, .. }) = dec.seek(Time::ZERO, SeekMode::Exact, &stop)
        else {
            panic!("seeking to the start");
        };
        let mut out = vec![frame.time];
        while let NextFrame::Frame(f) = dec.next_frame(&stop).unwrap() {
            out.push(f.time);
        }
        out
    }

    /// What the decode stage sends for `trim` at `rate`: each output
    /// frame's number and the time of the source frame held for it.
    fn held(path: &Path, trim: Range, rate: FrameRate) -> Result<Vec<(u64, Time)>, ExportError> {
        let plan = FramePlan::new(trim, rate).unwrap();
        let (tx, rx) = mpsc::sync_channel(plan.count() as usize);
        let ended = decode(open(path), plan, tx, StopToken::never());
        let got = rx.try_iter().map(|h| (h.n, h.frame.time)).collect();
        match ended {
            Ended::Done => Ok(got),
            Ended::Failed(e) => Err(e),
            Ended::Stopped | Ended::Downstream => panic!("the decode stage stopped"),
        }
    }

    /// For each output frame of `trim` at `rate`, the last source time at
    /// or before its own.
    fn want(source: &[Time], trim: Range, rate: FrameRate) -> Vec<(u64, Time)> {
        let plan = FramePlan::new(trim, rate).unwrap();
        (0..plan.count())
            .map(|n| {
                let t = plan.source_time(n).unwrap();
                let shown = source.iter().rev().find(|&&s| s <= t).unwrap();
                (n, *shown)
            })
            .collect()
    }

    /// How many output frames show a source frame shown before, and how
    /// many source frames between the first and last shown are never.
    fn repeats_and_skips(got: &[(u64, Time)], source: &[Time]) -> (usize, usize) {
        let repeats = got.windows(2).filter(|w| w[0].1 == w[1].1).count();
        let (first, last) = (got[0].1, got[got.len() - 1].1);
        let skips = source
            .iter()
            .filter(|&&s| s >= first && s <= last && !got.iter().any(|g| g.1 == s))
            .count();
        (repeats, skips)
    }

    #[test]
    fn a_steady_source_is_held_and_skipped_to_the_frame_shown_at_each_time() {
        let dir = TempDir::new("hold-cfr");
        // 30 frames at 30 fps.
        let path = synth::write(&Spec::default(), &dir.0).unwrap();
        let source = times(&path);
        assert_eq!(source.len(), 30);
        // Whether frames repeat, and whether some are skipped.
        let trims = [
            // Mid-GOP, between frames 7 and 8, at 24 fps: skipped.
            (secs(1, 4), secs(9, 10), FrameRate::FPS_24, (false, true)),
            // From the start at 60: repeated.
            (
                Time::ZERO,
                Time::from_secs(1),
                FrameRate::FPS_60,
                (true, false),
            ),
            // At 29.97, which falls behind 30 by a frame in a thousand:
            // each once in under half a second.
            (
                secs(1, 10),
                secs(1, 2),
                FrameRate::FPS_29_97,
                (false, false),
            ),
        ];
        for (start, end, rate, shape) in trims {
            let trim = Range { start, end };
            let got = held(&path, trim, rate).unwrap();
            assert_eq!(got, want(&source, trim, rate), "{trim:?} at {rate:?}");
            let (r, s) = repeats_and_skips(&got, &source);
            assert_eq!((r > 0, s > 0), shape, "{trim:?} at {rate:?}");
        }
        // Each frame twice at 60.
        let trim = Range {
            start: Time::ZERO,
            end: Time::from_secs(1),
        };
        let got = held(&path, trim, FrameRate::FPS_60).unwrap();
        assert_eq!(got.len(), 60);
        for (n, t) in &got {
            assert_eq!(*t, source[(*n / 2) as usize]);
        }
    }

    #[test]
    fn a_variable_rate_source_is_held_to_the_last_frame_at_or_before_each_time() {
        let dir = TempDir::new("hold-vfr");
        let spec = Spec {
            name: "vfr".into(),
            vfr: true,
            ..Spec::default()
        };
        let path = synth::write(&spec, &dir.0).unwrap();
        let source = times(&path);
        assert_eq!(source.len(), 30);
        // The frames last 1/30 to 1/15 s, as written.
        for (i, w) in source.windows(2).enumerate() {
            let d = w[1].checked_sub(w[0]).unwrap();
            assert_eq!(d, secs(vfr_ticks(i as u32), VFR_TIMESCALE), "frame {i}");
        }
        let end = *source.last().unwrap();
        for (start, rate) in [
            (Time::ZERO, FrameRate::FPS_30),
            (secs(3, 10), FrameRate::FPS_29_97),
            (secs(1, 7), FrameRate::FPS_24),
        ] {
            let trim = Range { start, end };
            let got = held(&path, trim, rate).unwrap();
            assert_eq!(got, want(&source, trim, rate), "{trim:?} at {rate:?}");
            // Slower than 30 for most of it: frames repeat.
            let (repeats, _) = repeats_and_skips(&got, &source);
            assert!(repeats > 0, "{trim:?} at {rate:?}");
        }
    }

    #[test]
    fn a_trim_past_the_last_frame_fails_at_the_first_frame_with_nothing_to_show() {
        let dir = TempDir::new("hold-past");
        let path = synth::write(&Spec::default(), &dir.0).unwrap();
        // The clip ends at 1 s: output frame 15 of a trim from 0.5 s
        // would show 1 s.
        let trim = Range {
            start: secs(1, 2),
            end: secs(3, 2),
        };
        let e = held(&path, trim, FrameRate::FPS_30).unwrap_err();
        let ExportError::Failed { stage, at, message } = e else {
            panic!("{e:?}");
        };
        assert_eq!((stage, at), (Stage::Decode, Some(secs(1, 2))), "{message}");
        assert!(message.contains("last frame ends at 1.000 s"), "{message}");
        // To the clip's end exactly is all there.
        let trim = Range {
            start: secs(1, 2),
            end: Time::from_secs(1),
        };
        assert_eq!(held(&path, trim, FrameRate::FPS_30).unwrap().len(), 15);
    }
}
