//! The export's three threads (M2 plan section 8.2): decode, render, and
//! encode and mux, joined by bounded channels and stopped together by
//! one [`StopSource`].
//!
//! The encode stage runs on the job's own thread and owns the writer;
//! decode and render get threads of their own. Each stage ends on its
//! own error, on the stop, or when the stage after it has gone (its
//! channel closed), and [`run`] reports the first stage that failed for
//! a reason of its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_core::brand::APP_NAME;
use spherecut_core::cancel::{StopSource, StopToken};
use spherecut_core::fs::{SWEEP_AGE, follow_links, free_space, sweep_temp_files};
use spherecut_core::project::{Codec, Output, Target, ViewAnim};
use spherecut_core::time::Time;
use spherecut_media::backend::{
    self, DecodeMode, DecodePath, DecoderEvent, Exactness, FallbackReason, FfmpegBackend, HwKind,
    MediaBackend, NextFrame, SeekMode, SeekOutcome, VideoDecode, VideoFrame, VideoRequest,
    VideoStreamInfo,
};
use spherecut_media::encode::{EncodeError, Mp4Writer, MuxOptions, StridedPlanes, VideoSettings};
use spherecut_media::hw::RenderNode;
use spherecut_media::inject::{EntryEdit, Finish, InjectError, TrackKind};
use spherecut_media::spherical::{StereoMode, write};
use spherecut_media::{HwDevice, capture_ffmpeg_log};
use spherecut_render::output::frame_views_at;
use spherecut_render::{
    Filter, Gpu, Placement, Stereo, StillOptions, StillRenderer, YuvImage, YuvLayout,
};

use crate::audio::{self, Feed};
use crate::check::{self, Expected, ExpectedAudio};
use crate::encoder::{EncoderKind, EncoderSetup};
use crate::error::{ExportError, Stage};
use crate::job::{
    AudioReport, DecodeReport, Decoder, Encoders, ExportJob, Fallback, Report, Shared, StageTimes,
    panic_text,
};
use crate::pace::Pace;
use crate::plan::FramePlan;
use crate::preset::Preset;
use crate::warning::{Warning, codes};

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
#[derive(Clone)]
struct Look {
    anim: ViewAnim,
    targets: Vec<Target>,
    output: Output,
    /// The size each frame renders at: the output's, or `--draft`'s.
    size: (u32, u32),
    blur: bool,
    opts: StillOptions,
    yuv: YuvLayout,
    pace: Option<Arc<Pace>>,
}

/// The whole export, on the job's thread: the decoder and the audio
/// chosen, then the video with each of `encoders` in turn until one
/// lasts past its first second (section 8.3), then the last pass and the
/// checks.
pub(crate) fn run(
    job: &ExportJob,
    preset: Preset,
    plan: FramePlan,
    node: &RenderNode,
    encoders: &Encoders,
    shared: &Shared,
    stop: &StopSource,
) -> Result<Report, ExportError> {
    let sequence = job.project.the_sequence();
    let clip = job.project.the_clip();
    let media = job
        .project
        .media_by_id(clip.media)
        .ok_or_else(|| ExportError::Invalid("the clip's media isn't in the project".into()))?;
    let folder = folder(&job.output)?;
    // What an export or a frame killed in this folder left (D16).
    match sweep_temp_files(&folder, SWEEP_AGE) {
        Ok(gone) => {
            for p in gone {
                tracing::info!("removed {}, which a stopped export left", p.display());
            }
        }
        Err(e) => tracing::debug!("no sweep of {}: {e}", folder.display()),
    }

    // `frame --decoder auto`'s choice (amendment 2 item 25).
    let backend = FfmpegBackend::on_node(node.clone());
    let (decode, decode_skipped) = decode_mode(job.decoder, node);
    if let Some(f) = &decode_skipped {
        shared.decode_fell_back(f.clone());
    }
    let req = VideoRequest {
        stream: Some(media.video.stream as usize),
        decode,
        interrupt: stop.shutdown_flag(),
        ..VideoRequest::default()
    };
    let open = || {
        backend.open_video(&media.path, &req).map_err(|e| {
            if stop.is_shut_down() {
                return ExportError::Cancelled;
            }
            ExportError::failed(
                Stage::Decode,
                None,
                format!("opening {}: {e}", media.path.display()),
            )
        })
    };
    let first = open()?;
    let info = first.info().clone();
    let mut dec = Some(first);
    let (placement, mut warnings) = placement(&info);
    let ten_bit = info.layout.bit_depth > 8 && !job.eight_bit;
    let s = &job.settings;
    let size = if job.draft {
        preset.draft_size()
    } else {
        preset.size()
    };
    let setup_of = |kind| {
        let setup = EncoderSetup::of(kind, s.codec, s.quality, preset, plan.rate(), ten_bit);
        let setup = if job.draft {
            setup.draft(preset.size(), size)
        } else {
            setup
        };
        match job.vaapi_level {
            Some(level) => setup.with_vaapi_level(level),
            None => setup,
        }
    };
    if s.codec == Codec::H264 && size.0 > H264_HARDWARE_WIDTH {
        warnings.push(Warning::new(
            codes::H264_LEVEL_6,
            format!(
                "H.264 at {}x{} needs level 6, which many players' hardware decoders can't \
                 play; HEVC or AV1 plays more widely at this size",
                size.0, size.1
            ),
        ));
    }
    let look = Look {
        anim: clip.view.clone(),
        targets: job.project.targets().to_vec(),
        output: sequence.output,
        size,
        blur: s.motion_blur,
        opts: StillOptions {
            filter: filter(&job.gpu),
            placement,
            ..StillOptions::default()
        },
        yuv: YuvLayout::I420,
        pace: job.pace.clone(),
    };
    let (audio_plan, audio_warnings) = audio::choose(&media.path, s.audio, &sequence.output)?;
    warnings.extend(audio_warnings);
    // The muxer's file and the finished one are both on the disk until
    // the first is removed (section 8.6, amendment 2 item 28). Every
    // encoder of a codec has the same rates.
    let needed = setup_of(encoders.kinds[0])
        .estimate(
            audio_plan.as_ref().map_or(0, audio::Plan::bits),
            plan.duration(),
        )
        .saturating_mul(2);
    warnings.extend(room(&folder, needed, free_space(&folder))?);
    let origin = match &audio_plan {
        None => None,
        Some(_) => Some(info.origin().map_err(|e| {
            ExportError::failed(Stage::Decode, None, format!("the video's first frame: {e}"))
        })?),
    };

    let mut fallback = encoders.skipped.clone();
    let mut kinds = encoders.kinds.iter().peekable();
    let (attempt, setup) = loop {
        let Some(&kind) = kinds.next() else {
            return Err(ExportError::NoEncoder("no encoder is left to try".into()));
        };
        let setup = setup_of(kind);
        shared.encoder(setup.name, fallback.clone());
        // A new generation: the stop for this try's threads alone, while
        // a cancel stops them all.
        let token = stop.next();
        let dec = match dec.take() {
            Some(d) => d,
            None => open()?,
        };
        let look = Look {
            yuv: setup.yuv,
            ..look.clone()
        };
        let tried = once(Once {
            job,
            setup: &setup,
            node,
            size,
            plan,
            look,
            dec,
            source: &media.path,
            audio: audio_plan.as_ref().zip(origin),
            token,
            shared,
            stop,
        });
        match tried {
            Ok(a) => break (a, setup),
            Err(Tried::Early { error, code }) if kinds.peek().is_some() && !stop.is_shut_down() => {
                tracing::warn!(
                    "{} failed, going on in the next encoder: {error}",
                    setup.name
                );
                fallback = Some(Fallback {
                    from: setup.name,
                    code,
                    reason: error.to_string(),
                });
            }
            // The hardware asked for, or the last there was: exit 3 in
            // the CLI, as frame's `--decoder vaapi` (D51).
            Err(Tried::Early { error, code })
                if kind == EncoderKind::Vaapi && !stop.is_shut_down() =>
            {
                return Err(ExportError::Hardware {
                    encoder: setup.name,
                    code,
                    message: error.to_string(),
                });
            }
            Err(Tried::Early { error, .. } | Tried::Late(error)) => return Err(error),
        }
    };
    let Attempt {
        writer,
        blur_capped,
        feed,
        decode,
        busy,
    } = attempt;
    let decode = DecodeReport {
        path: decode.path,
        fallback: decode.fallback.or(decode_skipped),
    };

    let mut edits = match sequence.output {
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
    if audio_plan.as_ref().is_some_and(|a| a.ambix) {
        edits.push(EntryEdit {
            track: TrackKind::Audio,
            boxes: write::sa3d_first_order_ambix(),
        });
    }
    warnings.extend(feed.as_ref().and_then(Feed::ended_early));
    // One last pass writes the file with its metadata, the moov first and
    // no video edit list (amendment 2 items 8 and 28).
    let finish = Finish {
        video_trak: match sequence.output {
            Output::Equirect { .. } => write::v1_equirect(APP_NAME),
            Output::Flat { .. } => Vec::new(),
        },
        drop_video_identity_edit: true,
        moov_first: true,
    };
    let mut progress = |done, total| {
        shared.finishing(done, total);
        !stop.is_shut_down()
    };
    let finishing = Instant::now();
    let tmp = writer
        .finish_temp(&edits, &finish, &mut progress)
        .map_err(|e| match e {
            _ if stop.is_shut_down() => ExportError::Cancelled,
            EncodeError::Inject(
                e @ (InjectError::Read { .. }
                | InjectError::Write { .. }
                | InjectError::Copy { .. }),
            ) => ExportError::failed(Stage::File, None, e.to_string()),
            EncodeError::Inject(e) => ExportError::failed(Stage::Metadata, None, e.to_string()),
            e => file_or(Stage::Mux, None, e),
        })?;
    if stop.is_shut_down() {
        return Err(ExportError::Cancelled);
    }
    let want = Expected {
        codec: s.codec,
        size,
        frames: plan.count(),
        duration: plan.duration(),
        rate: plan.rate(),
        sphere: matches!(sequence.output, Output::Equirect { .. }),
        audio: audio_plan.as_ref().map(|a| ExpectedAudio {
            codec: a.codec().to_owned(),
            rate: a.rate(),
            channels: a.channels(),
            ambix: a.ambix,
        }),
    };
    // A failed check drops `tmp`, which removes it.
    let checks = check::check(tmp.path(), &want, stop.shutdown_flag()).map_err(|e| {
        if stop.is_shut_down() {
            ExportError::Cancelled
        } else {
            e
        }
    })?;
    if stop.is_shut_down() {
        return Err(ExportError::Cancelled);
    }
    let stages = StageTimes {
        finish: finishing.elapsed(),
        ..busy
    };
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
        draft: job.draft,
        rate: plan.rate(),
        codec: s.codec,
        encoder: setup.name,
        fallback,
        decode,
        ten_bit: setup.ten_bit(),
        audio: audio_plan.zip(feed.as_ref()).map(|(a, f)| AudioReport {
            stream: a.stream,
            encoder: a.encoder(),
            rate: f.rate(),
            channels: a.channels(),
            ambix: a.ambix,
            samples: f.total(),
        }),
        bytes,
        blur_capped,
        warnings,
        checks,
        stages,
        elapsed: shared.elapsed(),
    })
}

/// The widest picture VA-API encodes in H.264 on AMD's VCN 4, the
/// kernel's limit (research note encode.md, section 2); above it H.264
/// goes to libx264 at level 6.
const H264_HARDWARE_WIDTH: u32 = 4096;

/// What `frame --decoder auto` would decode on (D39): VA-API on the
/// GPU's node going on in software, or software with the reason when
/// there is no node to try.
fn decode_mode(decoder: Decoder, node: &RenderNode) -> (DecodeMode, Option<Fallback>) {
    match decoder {
        Decoder::Software => (DecodeMode::Software, None),
        Decoder::Auto if !node.path.exists() => (
            DecodeMode::Software,
            Some(Fallback {
                from: HwKind::VaApi.ffmpeg_name(),
                code: "hw_unavailable",
                reason: format!(
                    "no VA-API device: no render node at {}",
                    node.path.display()
                ),
            }),
        ),
        Decoder::Auto => (
            DecodeMode::Hardware {
                kind: HwKind::VaApi,
                device: None,
                fallback: backend::Fallback::Software,
            },
            None,
        ),
    }
}

/// What one try at the video needs.
struct Once<'a> {
    job: &'a ExportJob,
    setup: &'a EncoderSetup,
    node: &'a RenderNode,
    size: (u32, u32),
    plan: FramePlan,
    look: Look,
    dec: Box<dyn VideoDecode>,
    /// The clip's file.
    source: &'a Path,
    /// The audio's plan and the video's first frame, which it is cut to.
    audio: Option<(&'a audio::Plan, Time)>,
    token: StopToken,
    shared: &'a Shared,
    stop: &'a StopSource,
}

/// A try that muxed every frame: the writer, still to be finished, the
/// count of frames whose blur was capped, the audio fed into it, and
/// where the frames were decoded.
struct Attempt {
    writer: Mp4Writer,
    blur_capped: u64,
    feed: Option<Feed>,
    decode: Seen,
    busy: StageTimes,
}

/// Each stage's busy time this try, in nanoseconds, added to by its
/// thread as it goes.
#[derive(Debug, Default)]
struct Busy {
    decode: AtomicU64,
    render: AtomicU64,
    readback: AtomicU64,
    encode: AtomicU64,
}

impl Busy {
    /// `f`, its time added to `stage`.
    fn time<T>(stage: &AtomicU64, f: impl FnOnce() -> T) -> T {
        let since = Instant::now();
        let t = f();
        Busy::add(stage, since.elapsed());
        t
    }

    fn add(stage: &AtomicU64, d: Duration) {
        let nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        stage.fetch_add(nanos, Ordering::Relaxed);
    }

    /// The times so far, with no finish.
    fn times(&self) -> StageTimes {
        let get = |a: &AtomicU64| Duration::from_nanos(a.load(Ordering::Relaxed));
        StageTimes {
            decode: get(&self.decode),
            render: get(&self.render),
            readback: get(&self.readback),
            encode: get(&self.encode),
            finish: Duration::ZERO,
        }
    }
}

/// How a try failed.
enum Tried {
    /// The encoder failed to open, or on a frame within its first second,
    /// with D27's code for it: the next encoder may try (section 8.3).
    Early {
        error: ExportError,
        code: &'static str,
    },
    /// Anything else, which fails the export.
    Late(ExportError),
}

/// One try: the hardware encoder's device, the audio, the decode and
/// render threads, and the encode stage on this one.
fn once(o: Once<'_>) -> Result<Attempt, Tried> {
    let Once {
        job,
        setup,
        node,
        size,
        plan,
        look,
        dec,
        source,
        audio,
        token,
        shared,
        stop,
    } = o;
    let device = match setup.kind {
        EncoderKind::Software => None,
        EncoderKind::Vaapi => {
            let node_path = node.path.to_string_lossy();
            match HwDevice::open(HwKind::VaApi.ffmpeg_name(), Some(&node_path)) {
                Ok(d) => Some(Arc::new(d)),
                Err(e) => {
                    return Err(Tried::Early {
                        error: ExportError::NoEncoder(format!(
                            "{}: no VA-API device at {}: {e}",
                            setup.name,
                            node.path.display()
                        )),
                        code: "hw_unavailable",
                    });
                }
            }
        }
    };
    let mut feed = match audio {
        None => None,
        Some((a, origin)) => {
            let opened = Feed::open(
                source,
                a,
                job.project.the_clip().source,
                origin,
                stop.shutdown_flag(),
                &token,
            );
            match opened {
                Err(_) if stop.is_shut_down() => return Err(Tried::Late(ExportError::Cancelled)),
                f => Some(f.map_err(Tried::Late)?),
            }
        }
    };
    let (held_tx, held_rx) = mpsc::sync_channel(DECODED_AHEAD);
    let (yuv_tx, yuv_rx) = mpsc::sync_channel(RENDERED_AHEAD);
    let busy = Arc::new(Busy::default());
    let decoding = spawn("export-decode", {
        let (token, busy) = (token.clone(), busy.clone());
        move || decode(dec, plan, held_tx, token, &busy)
    })
    .map_err(Tried::Late)?;
    let rendering = spawn("export-render", {
        let gpu = job.gpu.clone();
        let (token, busy) = (token.clone(), busy.clone());
        move || render(gpu, look, plan, held_rx, yuv_tx, token, &busy)
    })
    .map_err(Tried::Late)?;
    let encoded = encode(Encode {
        job,
        setup,
        device,
        size,
        plan,
        rx: yuv_rx,
        feed: feed.as_mut(),
        shared,
        stop,
        token: &token,
        busy: &busy,
    });
    let (decoded, seen) = decoding.join().unwrap_or_else(|p| {
        let e = ExportError::failed(Stage::Decode, None, panic_text(&*p));
        (Ended::Failed(e), Seen::default())
    });
    let rendered = join(rendering, Stage::Render);
    if stop.is_shut_down() {
        return Err(Tried::Late(ExportError::Cancelled));
    }
    match encoded {
        Err(Failed {
            error,
            encoder: Some(code),
            frames,
        }) if frames < u64::from(setup.keyint) => return Err(Tried::Early { error, code }),
        _ => {}
    }
    for ended in [decoded, rendered] {
        if let Ended::Failed(e) = ended {
            return Err(Tried::Late(e));
        }
    }
    let (writer, blur_capped) = encoded.map_err(|f| Tried::Late(f.error))?;
    if let Some(f) = &seen.fallback {
        shared.decode_fell_back(f.clone());
    }
    Ok(Attempt {
        writer,
        blur_capped,
        feed,
        decode: seen,
        busy: busy.times(),
    })
}

/// The folder the export's files go in: the output's, through any links
/// to it, as the writer finds it.
fn folder(output: &Path) -> Result<PathBuf, ExportError> {
    let dest = follow_links(output).map_err(|e| {
        ExportError::failed(Stage::File, None, format!("{}: {e}", output.display()))
    })?;
    Ok(match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    })
}

/// Whether `folder`, with `free` bytes, has room for `needed`: a refusal
/// with both numbers when it hasn't, and a warning when its free space
/// can't be read (a filesystem without `statvfs`), since the write will
/// say so if it is full.
fn room(
    folder: &Path,
    needed: u64,
    free: std::io::Result<u64>,
) -> Result<Option<Warning>, ExportError> {
    match free {
        Ok(free) if free < needed => Err(ExportError::NoSpace {
            folder: folder.to_path_buf(),
            needed,
            free,
        }),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ExportError::failed(
            Stage::File,
            None,
            format!("the output's folder {} doesn't exist", folder.display()),
        )),
        Err(e) => Ok(Some(Warning::new(
            codes::FREE_SPACE_UNKNOWN,
            format!("{}'s free space wasn't checked: {e}", folder.display()),
        ))),
    }
}

fn spawn<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<JoinHandle<T>, ExportError> {
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
fn placement(info: &VideoStreamInfo) -> (Placement, Vec<Warning>) {
    let m = &info.mapping;
    let mut warnings = Vec::new();
    let stereo = match m.stereo {
        StereoMode::Mono => Stereo::Mono,
        StereoMode::TopBottom => {
            warnings.push(Warning::new(
                codes::STEREO_LEFT_EYE,
                "stereo, top-bottom: exporting the left eye (the top half)",
            ));
            Stereo::TopBottom
        }
        StereoMode::LeftRight => {
            warnings.push(Warning::new(
                codes::STEREO_LEFT_EYE,
                "stereo, left-right: exporting the left eye (the left half)",
            ));
            Stereo::LeftRight
        }
        other => {
            warnings.push(Warning::new(
                codes::STEREO_UNKNOWN,
                format!("stereo layout {other:?} is unknown: exporting the whole picture, as mono"),
            ));
            Stereo::Mono
        }
    };
    let [u0, v0, u1, v1] = m.cover;
    if m.cover != [0.0, 0.0, 1.0, 1.0] {
        warnings.push(Warning::new(
            codes::CROP_APPLIED,
            "the picture covers part of the sphere: the rest is black",
        ));
    }
    if m.pose_present {
        warnings.push(Warning::new(
            codes::POSE_NOT_APPLIED,
            "the file's pose is not applied",
        ));
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

/// Where the decode stage's frames were decoded: the last frame's path,
/// and the first fallback to software (D3).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    path: &'static str,
    fallback: Option<Fallback>,
}

impl Default for Seen {
    fn default() -> Seen {
        Seen {
            path: SOFTWARE,
            fallback: None,
        }
    }
}

/// [`DecodeReport::path`] for software.
const SOFTWARE: &str = "software";

impl Seen {
    fn saw(&mut self, frame: &VideoFrame) {
        self.path = match &frame.decoded_on {
            DecodePath::Software { .. } => SOFTWARE,
            DecodePath::Hardware { kind } => kind.ffmpeg_name(),
            DecodePath::FellBack { from, reason } => {
                self.fallback.get_or_insert_with(|| Fallback {
                    from: from.ffmpeg_name(),
                    code: match reason {
                        FallbackReason::Refused(_) => "hw_unsupported",
                        FallbackReason::DeviceUnavailable(_) => "hw_unavailable",
                        FallbackReason::Failed { .. } => "hw_failed",
                    },
                    reason: reason.to_string(),
                });
                SOFTWARE
            }
        };
    }
}

/// The decode stage, and where its frames were decoded.
fn decode(
    dec: Box<dyn VideoDecode>,
    plan: FramePlan,
    tx: SyncSender<Held>,
    token: StopToken,
    busy: &Busy,
) -> (Ended, Seen) {
    let mut seen = Seen::default();
    let ended = decode_frames(dec, plan, tx, token, &mut seen, busy);
    (ended, seen)
}

/// The source frame shown at each output frame's time, the last with a
/// presentation time at or before it (frame hold), in order. A frame the
/// decoder can't give fails the export with its time.
fn decode_frames(
    mut dec: Box<dyn VideoDecode>,
    plan: FramePlan,
    tx: SyncSender<Held>,
    token: StopToken,
    seen: &mut Seen,
    busy: &Busy,
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
    let sought = Busy::time(&busy.decode, || dec.seek(start, SeekMode::Exact, &token));
    let mut cur = match sought {
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
    seen.saw(&cur);
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
                None => match Busy::time(&busy.decode, || dec.next_frame(&token)) {
                    Ok(NextFrame::Frame(f)) => {
                        seen.saw(&f);
                        f
                    }
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
    busy: &Busy,
) -> Ended {
    let mut renderer = StillRenderer::new(&gpu);
    let frame_s = plan.rate().frame_duration().to_secs_f64();
    let mut pass = 0;
    for held in rx {
        if token.is_stopped() {
            return Ended::Stopped;
        }
        // While the app's viewer plays, a frame a UI pass.
        if let Some(p) = &look.pace {
            p.wait(&mut pass);
        }
        let since = Instant::now();
        let t = plan.source_time(held.n).map_or(f64::NAN, Time::to_secs_f64);
        let views = frame_views_at(
            &look.anim,
            &look.targets,
            &look.output,
            look.size,
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
        let readback = renderer.last_readback();
        Busy::add(&busy.render, since.elapsed().saturating_sub(readback));
        Busy::add(&busy.readback, readback);
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

/// What the encode stage needs.
struct Encode<'a> {
    job: &'a ExportJob,
    setup: &'a EncoderSetup,
    /// The hardware encoder's device.
    device: Option<Arc<HwDevice>>,
    size: (u32, u32),
    plan: FramePlan,
    rx: Receiver<Rendered>,
    feed: Option<&'a mut Feed>,
    shared: &'a Shared,
    stop: &'a StopSource,
    /// This try's.
    token: &'a StopToken,
    busy: &'a Busy,
}

/// How the encode stage failed.
struct Failed {
    error: ExportError,
    /// When the encoder itself failed, D27's code for how: it didn't open
    /// (`hw_unavailable`, `hw_unsupported`), or failed on a frame
    /// (`hw_failed`).
    encoder: Option<&'static str>,
    /// Frames muxed before it failed.
    frames: u64,
}

impl From<ExportError> for Failed {
    fn from(error: ExportError) -> Failed {
        Failed {
            error,
            encoder: None,
            frames: 0,
        }
    }
}

/// The encode stage: every frame into the writer, in order, and after
/// each the audio up to its end. Returns the writer, all frames and
/// samples pushed, and the count of frames whose blur was capped.
fn encode(e: Encode<'_>) -> Result<(Mp4Writer, u64), Failed> {
    let Encode {
        job,
        setup,
        device,
        size: (width, height),
        plan,
        rx,
        mut feed,
        shared,
        stop,
        token,
        busy,
    } = e;
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
        device,
    };
    let mux = MuxOptions {
        movie_timescale: Some(feed.as_deref().map_or(audio::RATE, Feed::rate)),
        negative_cts_offsets: true,
    };
    let track = feed.as_deref().map(Feed::track);
    let (created, log) = Busy::time(&busy.encode, || {
        capture_ffmpeg_log(|| Mp4Writer::create_with(&job.output, &settings, track, &mux))
    });
    let mut writer = created.map_err(|e| {
        let code = match &e {
            EncodeError::NoEncoder(_) => Some("hw_unavailable"),
            EncodeError::Ffmpeg { .. } | EncodeError::Device(_) => Some("hw_unsupported"),
            _ => None,
        };
        Failed {
            error: writer_error(&job.output, setup.name, e, &log),
            encoder: code,
            frames: 0,
        }
    })?;
    // The audio up to export time `upto`, or all of it; a stop is the
    // cancel.
    let mut audio_to = |writer: &mut Mp4Writer, upto: Option<Time>| -> Result<(), ExportError> {
        match feed.as_deref_mut().map(|f| f.feed(writer, upto, token)) {
            Some(Ok(false)) => Err(ExportError::Cancelled),
            Some(Err(e)) => Err(e),
            Some(Ok(true)) | None => Ok(()),
        }
    };
    let mut capped = 0;
    let mut next = 0;
    while next < plan.count() {
        if stop.is_shut_down() {
            return Err(ExportError::Cancelled.into());
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
            )
            .into());
        }
        let spans: Vec<(usize, usize)> = r
            .image
            .shape
            .planes
            .iter()
            .map(|p| (p.offset as usize, p.stride as usize))
            .collect();
        let planes = StridedPlanes {
            size: r.image.shape.size,
            data: &r.image.data,
            planes: &spans,
        };
        #[cfg(test)]
        if fault::take(next) {
            return Err(Failed {
                error: ExportError::failed(Stage::Encode, at, "an injected encoder failure"),
                encoder: Some("hw_failed"),
                frames: next,
            });
        }
        let since = Instant::now();
        let pushed = writer.push_video_strided(&planes);
        #[cfg(test)]
        let pushed = pushed.and_then(|()| fault::disk(next));
        pushed.map_err(|e| encoder_failed(e, at, next))?;
        capped += u64::from(r.capped);
        next += 1;
        let end = plan
            .sequence_time(next)
            .map_err(|e| ExportError::failed(Stage::Encode, at, e.to_string()))?;
        audio_to(&mut writer, Some(end))?;
        Busy::add(&busy.encode, since.elapsed());
        shared.muxed(next);
    }
    if next < plan.count() {
        // `run` reports the stage that ended early; this is only what is
        // left if none did.
        return Err(ExportError::failed(
            Stage::Encode,
            plan.sequence_time(next).ok(),
            "the frames stopped coming",
        )
        .into());
    }
    // What the encoder still holds (VA-API's GOP) fails here as a frame
    // would: within its first second, the next encoder may try.
    #[cfg(test)]
    if fault::take(next) {
        return Err(Failed {
            error: ExportError::failed(Stage::Encode, None, "an injected failure at the flush"),
            encoder: Some("hw_failed"),
            frames: next,
        });
    }
    let since = Instant::now();
    writer
        .flush_video()
        .map_err(|e| encoder_failed(e, None, next))?;
    Busy::add(&busy.encode, since.elapsed());
    audio_to(&mut writer, None)?;
    Ok((writer, capped))
}

/// The writer failing on the frame at `at`, or at the flush, after
/// `frames` frames: the encoder's failure unless it was the file's.
fn encoder_failed(e: EncodeError, at: Option<Time>, frames: u64) -> Failed {
    let fault = !matches!(
        e,
        EncodeError::Io { .. } | EncodeError::Write(_) | EncodeError::Invalid(_)
    );
    Failed {
        error: file_or(Stage::Encode, at, e),
        encoder: fault.then_some("hw_failed"),
        frames,
    }
}

/// The unit tests' failures: the encode stage on this thread fails at
/// frame `n`, once, as a hardware encoder might, or its file stops
/// taking writes there, as a full disk does.
#[cfg(test)]
mod fault {
    use std::cell::Cell;

    use spherecut_media::encode::EncodeError;

    thread_local! {
        static AT: Cell<Option<u64>> = const { Cell::new(None) };
        static FULL_AT: Cell<Option<u64>> = const { Cell::new(None) };
    }

    pub(super) fn at(n: Option<u64>) {
        AT.with(|a| a.set(n));
    }

    pub(super) fn full_at(n: Option<u64>) {
        FULL_AT.with(|a| a.set(n));
    }

    /// The write of frame `n`: ENOSPC from then on.
    pub(super) fn disk(n: u64) -> Result<(), EncodeError> {
        match FULL_AT.with(Cell::get) {
            Some(at) if n >= at => Err(EncodeError::Write(std::io::Error::from_raw_os_error(28))),
            _ => Ok(()),
        }
    }

    /// Whether frame `n` fails now, which it does once.
    pub(super) fn take(n: u64) -> bool {
        AT.with(|a| {
            let hit = a.get() == Some(n);
            if hit {
                a.set(None);
            }
            hit
        })
    }
}

/// A writer that couldn't start: no encoder, or no file. `log` is what
/// FFmpeg said meanwhile, whose last line often says why an encoder
/// turned the job down ("Hardware does not support encoding at size").
fn writer_error(output: &Path, encoder: &str, e: EncodeError, log: &[String]) -> ExportError {
    match e {
        EncodeError::NoEncoder(name) => {
            ExportError::NoEncoder(format!("this FFmpeg has no {name}"))
        }
        EncodeError::Io { error, .. } => ExportError::failed(
            Stage::File,
            None,
            format!("creating a file beside {}: {error}", output.display()),
        ),
        e @ EncodeError::Write(_) => ExportError::failed(Stage::File, None, e.to_string()),
        e => {
            let why = log
                .last()
                .map(|l| format!(" ({})", l.trim()))
                .unwrap_or_default();
            ExportError::failed(Stage::Encode, None, format!("{encoder}: {e}{why}"))
        }
    }
}

/// `e` as a failure of `stage`, or of the file when it is the disk's.
fn file_or(stage: Stage, at: Option<Time>, e: EncodeError) -> ExportError {
    match e {
        EncodeError::Io { .. } | EncodeError::Write(_) => {
            ExportError::failed(Stage::File, at, e.to_string())
        }
        e => ExportError::failed(stage, at, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::file::head_hash;
    use spherecut_core::project::{
        Aspect, AudioChoice, Edit, EncoderChoice, ExportSettings, Fingerprint, InitialView, Media,
        MediaId, MediaVideo, Orientation, Project, Quality, Range,
    };
    use spherecut_core::time::{FrameRate, Rational};
    use spherecut_media::hw::NodeChoice;
    use spherecut_render::AdapterChoice;
    use spherecut_sphere::orient::Ypr;
    use spherecut_sphere::perspective::Projection;
    use spherecut_synth::media::{self as synth, Spec, VFR_TIMESCALE, vfr_ticks};

    use super::*;
    use crate::job::Decoder;

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
        let (ended, seen) = decode(open(path), plan, tx, StopToken::never(), &Busy::default());
        assert_eq!(seen, Seen::default(), "decoded in software");
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

    /// A job exporting `frames` frames of a 2 s grid clip at 30 fps as
    /// flat 1:1 1080 H.264 at Small, software decode, into `dir`.
    fn job(dir: &Path, frames: i64) -> ExportJob {
        let path = synth::write(
            &Spec {
                name: "two-seconds".into(),
                frames: 60,
                ..Spec::default()
            },
            dir,
        )
        .unwrap();
        let dec = open(&path);
        let info = dec.info();
        let media = Media {
            id: MediaId::NEW,
            path: path.clone(),
            fingerprint: Fingerprint {
                bytes: std::fs::metadata(&path).unwrap().len(),
                head: head_hash(&path).unwrap(),
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
            orientation: Orientation::from_ypr(Ypr::from_degrees(0.0, 0.0, 0.0)).unwrap(),
            fov_deg: 90.0,
            projection: Projection::Rectilinear,
        };
        let mut p = Project::untitled(media, view).unwrap();
        let (sequence, clip) = (p.the_sequence().id, p.the_clip().id);
        p.apply(&[
            Edit::SetTrim {
                clip,
                start: Some(Time::ZERO),
                end: Some(secs(frames, 30)),
            },
            Edit::SetOutput {
                sequence,
                output: Output::Flat {
                    aspect: Aspect::Square,
                    short_side: 1080,
                },
            },
            Edit::SetExport {
                sequence,
                export: ExportSettings {
                    codec: Codec::H264,
                    quality: Quality::Small,
                    encoder: EncoderChoice::Software,
                    audio: AudioChoice::None,
                    motion_blur: false,
                },
            },
        ])
        .unwrap();
        let gpu = Gpu::headless(&AdapterChoice::Cpu).unwrap();
        let mut job = ExportJob::new(p, dir.join("out.mp4"), gpu);
        job.decoder = Decoder::Software;
        job
    }

    /// `job` exported through `kinds`, on this thread, with frame
    /// `fail_at` failing in the first encoder.
    fn export_through(
        job: &ExportJob,
        kinds: Vec<EncoderKind>,
        fail_at: Option<u64>,
    ) -> (Result<Report, ExportError>, crate::job::Progress) {
        let sequence = job.project.the_sequence();
        let preset = Preset::of(&sequence.output).unwrap();
        let plan = FramePlan::new(job.project.the_clip().source, sequence.rate).unwrap();
        let node = RenderNode {
            path: job.output.with_file_name("no-render-node"),
            chosen: NodeChoice::Default,
        };
        let encoders = Encoders {
            kinds,
            skipped: None,
            names: ("h264_vaapi", "libx264"),
        };
        let shared = Shared::new(plan.count(), "libx264", None);
        fault::at(fail_at);
        let r = run(
            job,
            preset,
            plan,
            &node,
            &encoders,
            &shared,
            &StopSource::new(),
        );
        fault::at(None);
        (r, shared.progress())
    }

    #[test]
    fn an_encoder_failing_in_its_first_second_hands_over_to_the_next() {
        let dir = TempDir::new("fallback-early");
        let job = job(&dir.0, 15);
        let two = vec![EncoderKind::Software, EncoderKind::Software];
        let (r, progress) = export_through(&job, two.clone(), Some(3));
        let r = r.unwrap();
        assert_eq!((r.frames, r.encoder), (15, "libx264"));
        let f = r.fallback.expect("the first encoder's failure is reported");
        assert_eq!((f.from, f.code), ("libx264", "hw_failed"));
        assert!(f.reason.contains("injected"), "{}", f.reason);
        // At the flush, after the last frame: a short file's VA-API
        // failure comes there.
        std::fs::remove_file(&job.output).unwrap();
        let r = export_through(&job, two.clone(), Some(15)).0.unwrap();
        assert_eq!((r.frames, r.encoder), (15, "libx264"));
        let f = r.fallback.expect("the flush's failure is reported");
        assert!(f.reason.contains("at the flush"), "{}", f.reason);
        assert_eq!(progress.frames_done, 15);
        assert_eq!(progress.fallback.map(|f| f.code), Some("hw_failed"));
        assert!(job.output.exists());
        // The decode was asked to be software, so nothing fell back there.
        assert_eq!(
            r.decode,
            DecodeReport {
                path: "software",
                fallback: None
            }
        );
        // Said in the report's own fields, as frame's decode is (D3).
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);

        // VA-API with no device, then software: auto's order.
        std::fs::remove_file(&job.output).unwrap();
        let auto = vec![EncoderKind::Vaapi, EncoderKind::Software];
        let r = export_through(&job, auto, None).0.unwrap();
        assert_eq!(r.encoder, "libx264");
        let f = r.fallback.unwrap();
        assert_eq!((f.from, f.code), ("h264_vaapi", "hw_unavailable"));
        assert!(f.reason.contains("no VA-API device"), "{}", f.reason);
    }

    #[test]
    fn the_report_says_where_the_time_went() {
        let dir = TempDir::new("stages");
        let job = job(&dir.0, 15);
        let r = export_through(&job, vec![EncoderKind::Software], None)
            .0
            .unwrap();
        let s = r.stages;
        for (stage, busy) in [
            ("decode", s.decode),
            ("render", s.render),
            ("readback", s.readback),
            ("encode", s.encode),
            ("finish", s.finish),
        ] {
            assert!(busy > Duration::ZERO, "{stage} took no time: {s:?}");
            // Each stage is one thread's, so none is busy for longer than
            // the export took.
            assert!(busy <= r.elapsed, "{stage}: {s:?} in {:?}", r.elapsed);
        }
        assert!(s.render + s.readback <= r.elapsed, "{s:?}");
    }

    #[test]
    fn va_api_alone_failing_is_the_hardware_s_error() {
        let dir = TempDir::new("fallback-vaapi");
        let job = job(&dir.0, 15);
        let e = export_through(&job, vec![EncoderKind::Vaapi], None)
            .0
            .unwrap_err();
        let ExportError::Hardware {
            encoder,
            code,
            message,
        } = e
        else {
            panic!("{e}");
        };
        assert_eq!((encoder, code), ("h264_vaapi", "hw_unavailable"));
        assert!(message.contains("no VA-API device"), "{message}");
        assert!(!job.output.exists());
    }

    #[test]
    fn with_no_encoder_left_or_after_its_first_second_a_failure_fails_the_export() {
        let dir = TempDir::new("fallback-none");
        // The only encoder.
        let job = job(&dir.0, 15);
        let (r, _) = export_through(&job, vec![EncoderKind::Software], Some(3));
        let e = r.unwrap_err();
        assert_eq!(e.stage(), Some(Stage::Encode), "{e}");
        assert!(!job.output.exists());
        // Frame 30 is past the first second at 30 fps, frames 0 to 29.
        let job = self::job(&dir.0, 36);
        let two = vec![EncoderKind::Software, EncoderKind::Software];
        let (r, _) = export_through(&job, two.clone(), Some(30));
        assert!(r.unwrap_err().to_string().contains("injected"));
        assert!(!job.output.exists());
        // Frame 29 is within it.
        let (r, _) = export_through(&job, two, Some(29));
        assert_eq!(r.unwrap().fallback.map(|f| f.code), Some("hw_failed"));
    }

    #[test]
    fn a_disk_that_fills_part_way_fails_the_file_and_leaves_nothing() {
        let dir = TempDir::new("full");
        let job = job(&dir.0, 15);
        // With an encoder to hand over to, which a full disk isn't
        // reason to.
        let two = vec![EncoderKind::Software, EncoderKind::Software];
        fault::full_at(Some(5));
        let (r, progress) = export_through(&job, two, None);
        fault::full_at(None);
        let e = r.unwrap_err();
        assert_eq!(
            (e.stage(), e.code()),
            (Some(Stage::File), "io_error"),
            "{e}"
        );
        assert!(e.to_string().contains("No space left on device"), "{e}");
        assert!(progress.fallback.is_none(), "{:?}", progress.fallback);
        assert!(!job.output.exists());
        let left: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_str().is_some_and(spherecut_core::fs::is_temp_name))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn auto_decodes_in_software_with_the_reason_when_there_is_no_render_node() {
        let node = RenderNode {
            path: PathBuf::from("/nonexistent/renderD128"),
            chosen: NodeChoice::Default,
        };
        let (mode, skipped) = decode_mode(Decoder::Auto, &node);
        assert!(matches!(mode, DecodeMode::Software));
        let f = skipped.unwrap();
        assert_eq!((f.from, f.code), ("vaapi", "hw_unavailable"));
        assert!(f.reason.contains("/nonexistent/renderD128"), "{}", f.reason);
        assert!(matches!(
            decode_mode(Decoder::Software, &node),
            (DecodeMode::Software, None)
        ));
        // A node that is there is tried, going on in software.
        let dir = TempDir::new("node");
        let there = RenderNode {
            path: dir.0.clone(),
            chosen: NodeChoice::Given,
        };
        let (mode, skipped) = decode_mode(Decoder::Auto, &there);
        assert!(skipped.is_none());
        assert!(matches!(
            mode,
            DecodeMode::Hardware {
                kind: HwKind::VaApi,
                fallback: backend::Fallback::Software,
                ..
            }
        ));
    }

    #[test]
    fn room_refuses_with_both_numbers_and_warns_when_it_cant_tell() {
        let dir = Path::new("/media/card");
        assert_eq!(room(dir, 100, Ok(100)), Ok(None));
        let e = room(dir, 101, Ok(100)).unwrap_err();
        assert_eq!(
            e,
            ExportError::NoSpace {
                folder: dir.to_path_buf(),
                needed: 101,
                free: 100
            }
        );
        let e = room(dir, 1, Err(std::io::ErrorKind::NotFound.into())).unwrap_err();
        assert_eq!(e.stage(), Some(Stage::File));
        assert!(e.to_string().contains("doesn't exist"), "{e}");
        let unsupported = std::io::Error::from_raw_os_error(38);
        let w = room(dir, u64::MAX, Err(unsupported)).unwrap();
        assert!(w.is_some_and(
            |w| w.code == codes::FREE_SPACE_UNKNOWN && w.message.contains("wasn't checked")
        ));
        // The real call, on a folder that is there.
        let tmp = TempDir::new("room");
        assert!(free_space(&tmp.0).unwrap() > 0);
        assert_eq!(folder(&tmp.0.join("out.mp4")).unwrap(), tmp.0);
        assert_eq!(folder(Path::new("out.mp4")).unwrap(), PathBuf::from("."));
    }
}
