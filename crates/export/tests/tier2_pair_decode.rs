//! Tier 2: the export's pair decode (M3 plan 4.3's P-DEC, section 12):
//! two `HwOutput::Download` decoders on the lens tracks of a
//! dual-fisheye file as section 12's export will run them, serial (one
//! thread steps both) and a thread a lens, on VA-API, Vulkan Video and
//! software.
//!
//! - `bench_export_pair_decode`: the gated rows follow K1's rule. The
//!   8K30 pair of clip 9 (`handheld_flips`, for K1 and K4) and the
//!   synthetic 2×2880² at 29.97 (`pair-hevc8-2880-2997-noisy-20s`, K2's
//!   export half) decode 600 frames a lens, timed from both lenses'
//!   first frame with the opening reported apart, beside a VA-API H.264
//!   encode of a fixed 3840×2160 frame at 30 fps set up by
//!   `EncoderSetup::vaapi`, since the export's encoder shares the video
//!   engine (D56). Each runs once to warm up, then three times: the
//!   slower lens's median is the verdict, and a run under 30 fps fails
//!   it. A pass counts only when each run's encode took 29.5 frames a
//!   second or more over the decode's own timed window; a miss counts
//!   whatever the encode took, since a lighter load can only help the
//!   decode. The 8K25, 5.7K60 and 4K120 samples and the other two perf
//!   pairs decode once each, alone, up to 600 frames a lens; the 2×2880²
//!   at 59.94 is P only, since its spec asks x265 for no temporal layers,
//!   where the X5 codes every other picture as TSA_N, and a note says so
//!   beside its rows (`synth::media::PERF_PAIR_P_ONLY`).
//! - K3: frames are held as section 12 will hold them, per lens the
//!   decoder's current and next frame, `DECODED_AHEAD` in a bounded
//!   channel and one at the consumer, so 5 a lens and 10 a pair. Every
//!   run is a child process of its own (this binary again, running
//!   `pair_decode_child`), whose peak resident set's growth is measured
//!   as `testkit::memory` does, so that one run's freed heap can't hide
//!   the next one's growth. On clip 9 each path also decodes the pair,
//!   and lens 0 by itself, alone: without the encode, whose memory would
//!   count once in each. In these runs the consumer lets a pair go only
//!   once every channel is full, as a render slower than the decode
//!   would, so that all 10 frames are alive at once; the timed rows'
//!   consumer takes each pair as it comes. The pair passes if it grows
//!   at most twice as much as lens 0, plus 10 %, and K3 decides only
//!   when each run held all its frames at once.
//! - Each K's verdict is a note: Pass, the redirect the plan names, or
//!   not decided and why, on this machine whichever machine the plan
//!   wrote the K for. Where VA-API misses and Vulkan Video has no rows, a
//!   redirect past Redirect 1 needs doctor's word that Vulkan Video
//!   doesn't decode the clip's format here, or the device's refusal of
//!   the stream; otherwise the K is not decided. A verdict never fails
//!   the test. A run that fails does, once every row is recorded, unless
//!   it failed for a path the machine lacks, which is a note; but once a
//!   path has decoded here, a run on it that finds the device or decoder
//!   missing fails, as does a refusal of a clip it has decoded. When no K
//!   is decided the test skips, naming each.
//!
//! The Tier 1 tests run the same code in software on section 9's 2×64²
//! pairs: the frames held, the zip, K1's and K3's rules and each K's
//! verdict, a run in a child process, the encode beside, and the whole
//! bench on small clips.
//!
//! Budget (M3 plan 4.1 rule 5): 15 s for the Tier 1 tests, which each
//! prints its time against.
//!
//! Run the bench with `cargo xtask hw-test`; plain `cargo test` lists it
//! as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use common::{Scratch, media_dir};
use serde_json::{Value, json};
use spherecut_core::project::{Aspect, Codec, Quality};
use spherecut_core::{FrameRate, StopToken, Time};
use spherecut_export::{EncoderKind, EncoderSetup, Preset};
use spherecut_media::backend::{
    DecodeMode, DecodePath, Exactness, Fallback, FfmpegBackend, HwKind, MediaBackend, NextFrame,
    SeekMode, SeekOutcome, VideoDecode, VideoFrame, VideoRequest,
};
use spherecut_media::doctor::{self, CheckResult, DoctorReport};
use spherecut_media::encode::{Mp4Writer, PixelLayout, StridedPlanes, VideoSettings};
use spherecut_media::probe::{self, StreamKind};
use spherecut_media::{HwDevice, MediaError};
use spherecut_synth::media::{
    self as synth, PAIR_64_GOP_FAULT, PAIR_64_LENS_SHORT, PAIR_64_MATCHED, PERF_PAIR_2880_2997,
    PERF_PAIR_2880_5994, PERF_PAIR_3840_2997, PERF_PAIR_P_ONLY,
};
use spherecut_testkit::control::{at_least, at_most, must_fail};
use spherecut_testkit::memory::{CLEAR_REFS, peak_rss_growth_kib};
use spherecut_testkit::{bench, samples, tier2};

/// Pairs section 12's channel holds between the decode and the render.
const DECODED_AHEAD: usize = 2;

/// Frames of a lens alive at once in section 12's export: the decoder's
/// current and next, `DECODED_AHEAD` in the channel, and one at the
/// consumer.
const HELD_A_LENS: usize = held_a_lens(DECODED_AHEAD);

/// Frames of a lens alive at once with a channel of `ahead`.
const fn held_a_lens(ahead: usize) -> usize {
    2 + ahead + 1
}

/// How often a consumer that waits for every channel to fill looks
/// again.
const FILL_POLL: Duration = Duration::from_micros(250);

/// Frames a lens in a gated run (K1's rule), and the most in any run.
const FRAMES: u64 = 600;

/// K1's rule: the runs after the warm-up whose median is the verdict.
const GATED_RUNS: usize = 3;

/// K1's rule: the slower lens's median must reach 30 × 1.2 fps, and no
/// run may fall under 30.
const PASS_FPS: f64 = 36.0;
const RUN_FLOOR_FPS: f64 = 30.0;

/// K1's rule: the encode beside runs at 30 fps. A pass counts only when
/// each run's encode took at least this many frames a second over the
/// decode's timed window, which leaves a frame of rounding over a window
/// of a few seconds and catches an encode that fell behind; a miss
/// counts whatever the encode took.
const ENCODE_FLOOR_FPS: f64 = 29.5;

/// K3: the pair may grow twice as much as lens 0 by itself, plus 10 %.
const K3_FACTOR: f64 = 2.0 * 1.1;

/// The encode beside the gated rows: flat 4K30 H.264 at High, the
/// export's default.
const ENCODE_SIZE: (u32, u32) = (3840, 2160);
const ENCODE_PRESET: Preset = Preset::Flat {
    aspect: Aspect::Wide,
    short_side: 2160,
};

/// The longest a run in a child process may take: software at 8K30
/// takes about 50 s on odin.
const CHILD_LIMIT: Duration = Duration::from_secs(600);

/// The variable that hands a child process its run, as JSON.
const CHILD_RUN: &str = "SPHERECUT_PAIR_DECODE_RUN";

/// What the line carrying a child's result starts with.
const RESULT: &str = "pair-decode-result: ";

/// K1's redirects, and K2's and K4's, as the plan names them.
const REDIRECT_1: &str = "Redirect 1 (D52's update: pairs decode with Vulkan Video download \
                          wherever doctor says it decodes HEVC)";
const REDIRECT_2: &str = "Redirect 2 (M3.11b: the export decodes onto its own device through \
                          path B or A, never through RAM)";
const REDIRECT_HERE: &str = "Redirect (M3.11b gains this machine's row: the export decodes \
                             through path A onto its own device)";
const K3_LEVERS: &str = "Redirect (K3's levers in order: the software fallback's frame threads \
                         per decoder; the download pool's size; DECODED_AHEAD 2 → 1 for pairs; \
                         then M3.11b)";

/// The suite's budget in seconds.
const BUDGET_S: f64 = 15.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A Tier 1 test's clock, which prints as the test ends how long it took
/// and how long the suite has run, against the suite's budget.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// A decode path: the backend's device on the render node, each frame
/// downloaded (`HwOutput::Download`, the request's default), or
/// software.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum On {
    VaApi,
    Vulkan,
    Software,
}

impl On {
    const ALL: [On; 3] = [On::VaApi, On::Vulkan, On::Software];

    /// As the rows and verdicts say it.
    fn label(self) -> &'static str {
        match self {
            On::VaApi => "VA-API download",
            On::Vulkan => "Vulkan Video download",
            On::Software => "software",
        }
    }

    /// As a child's run names it.
    fn name(self) -> &'static str {
        match self {
            On::VaApi => "vaapi",
            On::Vulkan => "vulkan",
            On::Software => "software",
        }
    }

    fn named(name: &str) -> Option<On> {
        On::ALL.into_iter().find(|on| on.name() == name)
    }

    fn kind(self) -> Option<HwKind> {
        match self {
            On::VaApi => Some(HwKind::VaApi),
            On::Vulkan => Some(HwKind::Vulkan),
            On::Software => None,
        }
    }

    /// The request's mode. A device decode fails rather than falls back,
    /// so that a row is the path it names.
    fn mode(self) -> DecodeMode {
        match self.kind() {
            None => DecodeMode::Software,
            Some(kind) => DecodeMode::Hardware {
                kind,
                device: None,
                fallback: Fallback::Fail,
            },
        }
    }

    fn decodes_on(self, path: &DecodePath) -> bool {
        match (self.kind(), path) {
            (None, DecodePath::Software { .. }) => true,
            (Some(want), DecodePath::Hardware { kind }) => want == *kind,
            _ => false,
        }
    }
}

/// How the lenses' decoders are driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setup {
    /// One thread steps every decoder and hands on pairs through one
    /// channel.
    Serial,
    /// A thread a lens, each with a channel of its own, zipped by the
    /// consumer.
    Threaded,
}

impl Setup {
    const ALL: [Setup; 2] = [Setup::Serial, Setup::Threaded];

    fn label(self) -> &'static str {
        match self {
            Setup::Serial => "serial",
            Setup::Threaded => "a thread a lens",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Setup::Serial => "serial",
            Setup::Threaded => "threaded",
        }
    }

    fn named(name: &str) -> Option<Setup> {
        Setup::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// The encode beside a gated run: K1's VA-API H.264, or libx264 in Tier
/// 1, of a fixed frame of `size` at 30 fps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Beside {
    kind: EncoderKind,
    size: (u32, u32),
}

impl Beside {
    fn setup(self) -> EncoderSetup {
        EncoderSetup::of(
            self.kind,
            Codec::H264,
            Quality::High,
            ENCODE_PRESET,
            FrameRate::FPS_30,
            false,
        )
    }

    fn kind_name(self) -> &'static str {
        match self.kind {
            EncoderKind::Vaapi => "vaapi",
            EncoderKind::Software => "software",
        }
    }
}

/// One run: what a child process is told to do.
#[derive(Debug, Clone, PartialEq)]
struct RunSpec {
    file: PathBuf,
    /// The lens streams, both or lens 0's alone.
    streams: Vec<usize>,
    on: On,
    setup: Setup,
    /// Frames a lens.
    frames: u64,
    /// Each channel's bound: `DECODED_AHEAD` but in a control.
    ahead: usize,
    /// Whether the consumer lets each pair go only once every channel
    /// is full, as a render slower than the decode would (K3's runs);
    /// otherwise it takes each pair as it comes.
    fill: bool,
    /// The render node the devices open on; the backend's default when
    /// `None`.
    node: Option<PathBuf>,
    beside: Option<Beside>,
    /// The folder the encode beside writes its file into.
    scratch: PathBuf,
}

/// A path as JSON, its bytes, so that one that isn't UTF-8 goes through.
fn path_json(p: &Path) -> Value {
    json!(p.as_os_str().as_bytes())
}

fn json_path(v: &Value) -> Option<PathBuf> {
    let bytes = v
        .as_array()?
        .iter()
        .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
        .collect::<Option<Vec<u8>>>()?;
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

impl RunSpec {
    fn to_json(&self) -> Value {
        json!({
            "file": path_json(&self.file),
            "streams": self.streams,
            "on": self.on.name(),
            "setup": self.setup.name(),
            "frames": self.frames,
            "ahead": self.ahead,
            "fill": self.fill,
            "node": self.node.as_deref().map(path_json),
            "beside": self.beside.map(|b| json!({
                "kind": b.kind_name(),
                "size": [b.size.0, b.size.1],
            })),
            "scratch": path_json(&self.scratch),
        })
    }

    fn from_json(v: &Value) -> Option<RunSpec> {
        let beside = match &v["beside"] {
            Value::Null => None,
            b => Some(Beside {
                kind: match b["kind"].as_str()? {
                    "vaapi" => EncoderKind::Vaapi,
                    "software" => EncoderKind::Software,
                    _ => return None,
                },
                size: (
                    u32::try_from(b["size"][0].as_u64()?).ok()?,
                    u32::try_from(b["size"][1].as_u64()?).ok()?,
                ),
            }),
        };
        Some(RunSpec {
            file: json_path(&v["file"])?,
            streams: v["streams"]
                .as_array()?
                .iter()
                .map(|s| s.as_u64().and_then(|s| usize::try_from(s).ok()))
                .collect::<Option<_>>()?,
            on: On::named(v["on"].as_str()?)?,
            setup: Setup::named(v["setup"].as_str()?)?,
            frames: v["frames"].as_u64()?,
            ahead: usize::try_from(v["ahead"].as_u64()?).ok()?,
            fill: v["fill"].as_bool()?,
            node: match &v["node"] {
                Value::Null => None,
                n => Some(json_path(n)?),
            },
            beside,
            scratch: json_path(&v["scratch"])?,
        })
    }
}

/// What a run measured.
#[derive(Debug, Clone, PartialEq)]
struct Ran {
    /// From asking for the first decoder until every lens's first frame
    /// was in hand, in seconds.
    open_s: f64,
    lenses: Vec<LensRan>,
    /// The most decoded frames alive at once.
    peak_held: usize,
    /// The bytes of a decoded frame, lens 0's first.
    frame_bytes: usize,
    encode: Option<Encoded>,
    /// How far the process's peak resident set rose over the run, in
    /// KiB, where the peak can be reset.
    rss_kib: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LensRan {
    frames: u64,
    /// From both lenses' first frame to this lens's last.
    secs: f64,
}

impl LensRan {
    /// Frames a second after the first, at which the clock starts.
    fn fps(self) -> f64 {
        if self.frames > 1 {
            (self.frames - 1) as f64 / self.secs
        } else {
            f64::NAN
        }
    }
}

/// The encode beside, over the decode's timed window: the frames it
/// took from both lenses' first frame to the last frame decoded, and how
/// long that was, so that frames it took while the decoders opened, or
/// caught up on after a stall once the decode was over, don't count.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Encoded {
    frames: u64,
    secs: f64,
}

impl Encoded {
    fn fps(self) -> f64 {
        if self.secs > 0.0 {
            self.frames as f64 / self.secs
        } else {
            f64::NAN
        }
    }
}

/// When the encode beside took each frame.
#[derive(Debug, Clone, Default, PartialEq)]
struct Taken(Vec<Instant>);

impl Taken {
    /// The frames taken after `from` up to `to`, over that window.
    fn over(&self, (from, to): (Instant, Instant)) -> Encoded {
        let frames = self.0.iter().filter(|&&t| t > from && t <= to).count();
        Encoded {
            frames: frames as u64,
            secs: to.saturating_duration_since(from).as_secs_f64(),
        }
    }
}

impl Ran {
    /// The slower lens's rate; NaN if any lens's is.
    fn slower_fps(&self) -> f64 {
        self.lenses
            .iter()
            .map(|l| l.fps())
            .reduce(|a, b| {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    a.min(b)
                }
            })
            .unwrap_or(f64::NAN)
    }

    fn to_json(&self) -> Value {
        json!({
            "open_s": self.open_s,
            "lenses": self.lenses.iter().map(|l| json!([l.frames, l.secs])).collect::<Vec<_>>(),
            "peak_held": self.peak_held,
            "frame_bytes": self.frame_bytes,
            "encode": self.encode.map(|e| json!([e.frames, e.secs])),
            "rss_kib": self.rss_kib,
        })
    }

    fn from_json(v: &Value) -> Option<Ran> {
        let pair = |v: &Value| Some((v[0].as_u64()?, v[1].as_f64()?));
        Some(Ran {
            open_s: v["open_s"].as_f64()?,
            lenses: v["lenses"]
                .as_array()?
                .iter()
                .map(|l| pair(l).map(|(frames, secs)| LensRan { frames, secs }))
                .collect::<Option<_>>()?,
            peak_held: usize::try_from(v["peak_held"].as_u64()?).ok()?,
            frame_bytes: usize::try_from(v["frame_bytes"].as_u64()?).ok()?,
            encode: match &v["encode"] {
                Value::Null => None,
                e => pair(e).map(|(frames, secs)| Encoded { frames, secs }),
            },
            rss_kib: v["rss_kib"].as_u64(),
        })
    }
}

/// Why a run has no result.
#[derive(Debug, Clone, PartialEq)]
enum NoRun {
    /// The path has no device or decoder on this machine (`Hardware`,
    /// `NoDecoder`). Its rows become a note, unless the path has decoded
    /// here before ([`Measured::judge`]).
    Unavailable(String),
    /// The device turned the clip's stream down (`HwUnsupported`), a
    /// reason of the stream's own. Its rows become a note, unless the path
    /// has decoded the clip before.
    Refused(String),
    /// Anything else, which fails the bench once every row is recorded.
    Failed(String),
}

impl NoRun {
    fn why(&self) -> &str {
        match self {
            NoRun::Unavailable(why) | NoRun::Refused(why) | NoRun::Failed(why) => why,
        }
    }
}

impl From<MediaError> for NoRun {
    fn from(e: MediaError) -> NoRun {
        match &e {
            MediaError::HwUnsupported { .. } => NoRun::Refused(e.to_string()),
            MediaError::Hardware { .. } | MediaError::NoDecoder { .. } => {
                NoRun::Unavailable(e.to_string())
            }
            _ => NoRun::Failed(e.to_string()),
        }
    }
}

/// Every run that has decoded on a path on this machine: the path, the
/// clip and the run's name.
#[derive(Debug, Default)]
struct Measured(Vec<(On, String, String)>);

impl Measured {
    fn add(&mut self, on: On, clip: &str, run: &str) {
        self.0.push((on, clip.to_string(), run.to_string()));
    }

    /// `no`, why a run on `on` over `clip` has no result, as what the
    /// path has decoded here makes it. A path that has decoded a clip has
    /// its device and decoder, so a later `Unavailable` is a failure; a
    /// device that has decoded this clip's streams can't then turn them
    /// down, so a later `Refused` is one too. A device may still refuse a
    /// clip it hasn't decoded, as one mode's level can be past it.
    fn judge(&self, on: On, clip: &str, no: NoRun) -> NoRun {
        let before = |same_clip: bool| {
            self.0
                .iter()
                .find(|(o, c, _)| *o == on && (!same_clip || c == clip))
                .map(|(_, _, run)| run)
        };
        let first = match &no {
            NoRun::Unavailable(_) => before(false),
            NoRun::Refused(_) => before(true),
            NoRun::Failed(_) => None,
        };
        match first {
            Some(run) => NoRun::Failed(format!(
                "`{run}` measured on this machine; this run: {}",
                no.why()
            )),
            None => no,
        }
    }
}

/// How many decoded frames are alive, and the most there have been.
#[derive(Debug, Default)]
struct Live {
    now: AtomicUsize,
    peak: AtomicUsize,
    /// Whether a decoder has read the last frame it will, or a decode
    /// thread has ended: after either the channels can't fill again.
    done: AtomicBool,
}

impl Live {
    /// Waits until `full` frames are alive or the decoders are done.
    fn wait_full(&self, full: usize) {
        while self.now.load(Ordering::SeqCst) < full && !self.done.load(Ordering::SeqCst) {
            thread::sleep(FILL_POLL);
        }
    }
}

/// Marks its run's decoders done in [`Live`] when dropped, however its
/// decode thread ends.
struct Ending<'a>(&'a Live);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::SeqCst);
    }
}

/// A decoded frame, counted in its run's [`Live`] while it lives.
struct Held {
    frame: Arc<VideoFrame>,
    live: Arc<Live>,
}

impl Held {
    fn new(frame: Arc<VideoFrame>, live: &Arc<Live>) -> Held {
        let now = live.now.fetch_add(1, Ordering::SeqCst) + 1;
        live.peak.fetch_max(now, Ordering::SeqCst);
        Held {
            frame,
            live: Arc::clone(live),
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.live.now.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One lens's decoder as section 12's decode drives it: the frame it
/// hands on next, and the one after it, read while the first is held.
struct Lens<'a> {
    k: usize,
    on: On,
    dec: Box<dyn VideoDecode>,
    next: Option<Held>,
    handed: u64,
    frames: u64,
    live: &'a Arc<Live>,
    /// When it decoded the last frame it read.
    read_at: Instant,
}

/// Lens `k` of `spec`, opened, at its first frame, which it holds; and
/// that frame's bytes.
fn open_lens<'a>(
    backend: &FfmpegBackend,
    spec: &RunSpec,
    k: usize,
    live: &'a Arc<Live>,
) -> Result<(Lens<'a>, usize), NoRun> {
    let req = VideoRequest {
        stream: Some(spec.streams[k]),
        decode: spec.on.mode(),
        ..VideoRequest::default()
    };
    let mut dec = backend.open_video(&spec.file, &req)?;
    // The export seeks exactly to the trim's start, here the clip's.
    let first = match dec.seek(Time::ZERO, SeekMode::Exact, &StopToken::never())? {
        SeekOutcome::Frame {
            frame,
            exactness: Exactness::Exact | Exactness::BeforeFirst,
            ..
        } => frame,
        SeekOutcome::Frame { exactness, .. } => {
            return Err(NoRun::Failed(format!(
                "lens {k}: the seek to its first frame came back {exactness:?}"
            )));
        }
        SeekOutcome::Stopped => return Err(NoRun::Failed(format!("lens {k}: the seek stopped"))),
    };
    let bytes = first.bytes();
    let mut lens = Lens {
        k,
        on: spec.on,
        dec,
        next: None,
        handed: 0,
        frames: spec.frames,
        live,
        read_at: Instant::now(),
    };
    lens.next = Some(lens.checked(first)?);
    Ok((lens, bytes))
}

impl Lens<'_> {
    /// The next frame to hand on, once the one after it is read; `None`
    /// once `frames` are handed on.
    fn step(&mut self) -> Result<Option<Held>, NoRun> {
        let Some(cur) = self.next.take() else {
            return Ok(None);
        };
        self.handed += 1;
        if self.handed < self.frames {
            self.next = Some(self.read()?);
        } else {
            // From here it holds only the frame it hands on.
            self.live.done.store(true, Ordering::SeqCst);
        }
        Ok(Some(cur))
    }

    fn read(&mut self) -> Result<Held, NoRun> {
        match self.dec.next_frame(&StopToken::never())? {
            NextFrame::Frame(f) => {
                self.read_at = Instant::now();
                self.checked(f)
            }
            NextFrame::End => Err(NoRun::Failed(format!(
                "lens {} ended after {} frames of {}",
                self.k, self.handed, self.frames
            ))),
            NextFrame::Stopped => Err(NoRun::Failed(format!("lens {} stopped", self.k))),
        }
    }

    /// `frame`, counted, if it was decoded on the path asked for and is
    /// whole.
    fn checked(&self, frame: Arc<VideoFrame>) -> Result<Held, NoRun> {
        let at = frame.time.to_secs_f64();
        if !self.on.decodes_on(&frame.decoded_on) {
            return Err(NoRun::Failed(format!(
                "lens {}'s frame at {at:.3} s was decoded on {:?}, not {}",
                self.k,
                frame.decoded_on,
                self.on.label()
            )));
        }
        if frame.concealed {
            return Err(NoRun::Failed(format!(
                "lens {}'s frame at {at:.3} s is damaged (the decoder concealed errors)",
                self.k
            )));
        }
        Ok(Held::new(frame, self.live))
    }

    /// What it decoded, timed from `t0`.
    fn ran(&self, t0: Instant) -> LensRan {
        LensRan {
            frames: self.handed,
            secs: self.read_at.saturating_duration_since(t0).as_secs_f64(),
        }
    }
}

/// Section 12's zip: pair `n`'s frames are at one time, or the lenses
/// are apart (`ExportError::LensesApart`).
fn same_time(n: u64, pair: &[Held]) -> Result<(), String> {
    let times: Vec<Time> = pair.iter().map(|h| h.frame.time).collect();
    if times.windows(2).all(|w| w[0] == w[1]) {
        return Ok(());
    }
    let said: Vec<String> = times
        .iter()
        .map(|t| format!("{:.6} s", t.to_secs_f64()))
        .collect();
    Err(format!(
        "pair {n}: the lenses are apart, at {}",
        said.join(" and ")
    ))
}

/// The consumer: takes each pair, holds its lenses to one time, and lets
/// it go before taking the next. When `spec.fill`, it holds each pair
/// until every frame section 12 holds is alive, with each channel full
/// and each decoder at its current and next frame, or until the
/// decoders are done. Returns how many pairs it took.
fn consume(
    mut next: impl FnMut() -> Option<Vec<Held>>,
    spec: &RunSpec,
    live: &Live,
) -> Result<u64, NoRun> {
    let full = held_a_lens(spec.ahead) * spec.streams.len();
    let mut n = 0;
    while let Some(pair) = next() {
        same_time(n, &pair).map_err(NoRun::Failed)?;
        if spec.fill {
            live.wait_full(full);
        }
        drop(pair);
        n += 1;
    }
    Ok(n)
}

/// What the decoders came to.
struct Decoded {
    open_s: f64,
    lenses: Vec<LensRan>,
    frame_bytes: usize,
    /// The decode's timed window: from both lenses' first frame to the
    /// last frame decoded.
    window: (Instant, Instant),
}

/// One thread opens every lens, steps them in turn, and hands on each
/// pair through one channel of `spec.ahead` pairs.
fn serial(spec: &RunSpec, backend: &FfmpegBackend, live: &Arc<Live>) -> Result<Decoded, NoRun> {
    let began = Instant::now();
    let (tx, rx) = mpsc::sync_channel::<Vec<Held>>(spec.ahead);
    thread::scope(|s| {
        let decoder = s.spawn(move || -> Result<Decoded, NoRun> {
            let _ending = Ending(live);
            let mut lenses = Vec::new();
            let mut frame_bytes = 0;
            for k in 0..spec.streams.len() {
                let (lens, bytes) = open_lens(backend, spec, k, live)?;
                if k == 0 {
                    frame_bytes = bytes;
                }
                lenses.push(lens);
            }
            let t0 = Instant::now();
            'pairs: loop {
                let mut pair = Vec::with_capacity(lenses.len());
                for lens in &mut lenses {
                    match lens.step()? {
                        Some(f) => pair.push(f),
                        None => break 'pairs,
                    }
                }
                if tx.send(pair).is_err() {
                    break;
                }
            }
            let last = lenses.iter().map(|l| l.read_at).fold(t0, Instant::max);
            Ok(Decoded {
                open_s: t0.duration_since(began).as_secs_f64(),
                lenses: lenses.iter().map(|l| l.ran(t0)).collect(),
                frame_bytes,
                window: (t0, last),
            })
        });
        // The receiver goes with the consumer, so that a consumer that
        // stops lets a blocked decoder go.
        let consumed = consume(move || rx.recv().ok(), spec, live);
        let decoded = decoder
            .join()
            .unwrap_or_else(|_| Err(NoRun::Failed("the decode thread panicked".into())))?;
        took_all(consumed?, spec)?;
        Ok(decoded)
    })
}

/// A thread a lens opens its decoder, waits for every other lens's
/// first frame, and steps it into a channel of its own of `spec.ahead`
/// frames; the consumer zips them.
fn threaded(spec: &RunSpec, backend: &FfmpegBackend, live: &Arc<Live>) -> Result<Decoded, NoRun> {
    let began = Instant::now();
    let opened = Barrier::new(spec.streams.len());
    thread::scope(|s| {
        let mut rxs = Vec::new();
        let mut decoders = Vec::new();
        for k in 0..spec.streams.len() {
            let (tx, rx) = mpsc::sync_channel::<Held>(spec.ahead);
            rxs.push(rx);
            let opened = &opened;
            decoders.push(s.spawn(move || -> Result<Threaded, NoRun> {
                let _ending = Ending(live);
                let lens = open_lens(backend, spec, k, live);
                // Every lens's first frame is in hand before any decodes
                // on: the clock starts there.
                opened.wait();
                let (mut lens, bytes) = lens?;
                let t0 = Instant::now();
                while let Some(f) = lens.step()? {
                    if tx.send(f).is_err() {
                        break;
                    }
                }
                Ok(Threaded {
                    ran: lens.ran(t0),
                    open_s: t0.duration_since(began).as_secs_f64(),
                    bytes,
                    window: (t0, lens.read_at.max(t0)),
                })
            }));
        }
        let consumed = consume(
            move || rxs.iter().map(|rx| rx.recv().ok()).collect(),
            spec,
            live,
        );
        let lenses = decoders
            .into_iter()
            .map(|d| {
                d.join().unwrap_or_else(|_| {
                    Err(NoRun::Failed("a lens's decode thread panicked".into()))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        took_all(consumed?, spec)?;
        let first = lenses.iter().map(|l| l.window.0).min();
        let last = lenses.iter().map(|l| l.window.1).max();
        let (Some(first), Some(last)) = (first, last) else {
            return Err(NoRun::Failed("the run had no lens".into()));
        };
        Ok(Decoded {
            open_s: lenses.iter().map(|l| l.open_s).fold(0.0, f64::max),
            frame_bytes: lenses[0].bytes,
            lenses: lenses.iter().map(|l| l.ran).collect(),
            window: (first, last),
        })
    })
}

/// What one lens's thread came to in [`threaded`].
struct Threaded {
    ran: LensRan,
    open_s: f64,
    bytes: usize,
    /// From its clock's start to its last frame.
    window: (Instant, Instant),
}

/// The consumer took every pair the run decoded.
fn took_all(consumed: u64, spec: &RunSpec) -> Result<(), NoRun> {
    if consumed == spec.frames {
        return Ok(());
    }
    Err(NoRun::Failed(format!(
        "the consumer took {consumed} pairs of {}",
        spec.frames
    )))
}

/// A frame for the encoder: noise in the luma and grey chroma, its
/// planes one after the other as `layout` lays them out, and each
/// plane's first byte and row length.
fn fixed_frame(layout: PixelLayout, (w, h): (u32, u32)) -> (Vec<u8>, Vec<(usize, usize)>) {
    let mut data = Vec::new();
    let mut planes = Vec::new();
    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    for (k, (row, rows)) in layout.plane_sizes(w, h).into_iter().enumerate() {
        planes.push((data.len(), row));
        if k == 0 {
            data.extend((0..row * rows).map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 56) as u8
            }));
        } else {
            data.resize(data.len() + row * rows, 128);
        }
    }
    (data, planes)
}

/// The encode beside: `b`'s encoder takes the same frame at 30 fps, or
/// as fast as it can below that, catching up after a stall, until
/// `stop`, and its file is removed. `started` is told once the encoder is
/// open; it takes at least one frame. Returns when it took each.
fn encode_beside(
    b: Beside,
    node: Option<&Path>,
    dir: &Path,
    stop: &AtomicBool,
    started: mpsc::Sender<()>,
) -> Result<Taken, String> {
    let setup = b.setup();
    let device = match setup.kind {
        EncoderKind::Software => None,
        EncoderKind::Vaapi => {
            let node = node.ok_or("no render node for the VA-API encoder")?;
            let node = node
                .to_str()
                .ok_or_else(|| format!("the render node {} isn't UTF-8", node.display()))?;
            let dev = HwDevice::open(HwKind::VaApi.ffmpeg_name(), Some(node))
                .map_err(|e| format!("{}: no VA-API device at {node}: {e}", setup.name))?;
            Some(Arc::new(dev))
        }
    };
    let settings = VideoSettings {
        encoder: setup.name.into(),
        width: b.size.0,
        height: b.size.1,
        rate: FrameRate::FPS_30,
        layout: setup.layout,
        options: setup.options.clone(),
        color: setup.color,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
        device,
    };
    let path = dir.join(format!("beside-{}.mp4", std::process::id()));
    let mut writer = Mp4Writer::create(&path, &settings, None)
        .map_err(|e| format!("{} at {}×{}: {e}", setup.name, b.size.0, b.size.1))?;
    let (data, planes) = fixed_frame(setup.layout, b.size);
    let frame = StridedPlanes {
        size: b.size,
        data: &data,
        planes: &planes,
    };
    started.send(()).ok();
    let began = Instant::now();
    let mut taken = Taken::default();
    let pushed = loop {
        if let Err(e) = writer.push_video_strided(&frame) {
            break Err(format!("{} at frame {}: {e}", setup.name, taken.0.len()));
        }
        taken.0.push(Instant::now());
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        let due = began + Duration::from_secs_f64(taken.0.len() as f64 / 30.0);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
    };
    let finished = writer.finish();
    std::fs::remove_file(&path).ok();
    pushed?;
    finished.map_err(|e| format!("finishing {}: {e}", setup.name))?;
    Ok(taken)
}

/// `b` if its encoder opens and takes a frame here, else why not.
fn probe_beside(b: Beside, node: Option<&Path>, dir: &Path) -> Result<Beside, String> {
    let stop = AtomicBool::new(true);
    let (started, _) = mpsc::channel();
    encode_beside(b, node, dir, &stop, started).map(|_| b)
}

/// One run as `spec` says, in this process: the encode beside started
/// first when there is one, then the decode, then the encode stopped.
fn run(spec: &RunSpec) -> Result<Ran, NoRun> {
    let backend = match &spec.node {
        Some(node) => FfmpegBackend::on_render_node(node.clone()),
        None => FfmpegBackend::new(),
    };
    let live = Arc::new(Live::default());
    let stop = AtomicBool::new(false);
    thread::scope(|s| {
        let encoder = match spec.beside {
            None => None,
            Some(b) => {
                let (started, has_started) = mpsc::channel();
                let (node, stop) = (spec.node.as_deref(), &stop);
                let encoder = s.spawn(move || encode_beside(b, node, &spec.scratch, stop, started));
                if has_started.recv().is_err() {
                    let why = match encoder.join() {
                        Ok(Err(why)) => why,
                        Ok(Ok(_)) => "it ended before it began".into(),
                        Err(_) => "it panicked".into(),
                    };
                    return Err(NoRun::Failed(format!(
                        "the encode beside didn't start: {why}"
                    )));
                }
                Some(encoder)
            }
        };
        let decoded = match spec.setup {
            Setup::Serial => serial(spec, &backend, &live),
            Setup::Threaded => threaded(spec, &backend, &live),
        };
        stop.store(true, Ordering::SeqCst);
        let encode = encoder.map(|e| {
            e.join()
                .unwrap_or_else(|_| Err("it panicked".into()))
                .map_err(|why| NoRun::Failed(format!("the encode beside: {why}")))
        });
        let decoded = decoded?;
        Ok(Ran {
            open_s: decoded.open_s,
            lenses: decoded.lenses,
            peak_held: live.peak.load(Ordering::SeqCst),
            frame_bytes: decoded.frame_bytes,
            encode: encode.transpose()?.map(|t| t.over(decoded.window)),
            rss_kib: None,
        })
    })
}

/// [`run`], with how far the process's peak resident set rose over it.
fn measured(spec: &RunSpec) -> Result<Ran, NoRun> {
    let mut ran = Err(NoRun::Failed("the run never ran".into()));
    let grew = peak_rss_growth_kib(Path::new(CLEAR_REFS), || ran = run(spec));
    ran.map(|r| Ran { rss_kib: grew, ..r })
}

fn result_json(r: &Result<Ran, NoRun>) -> Value {
    match r {
        Ok(ran) => json!({ "ok": ran.to_json() }),
        Err(NoRun::Unavailable(why)) => json!({ "unavailable": why }),
        Err(NoRun::Refused(why)) => json!({ "refused": why }),
        Err(NoRun::Failed(why)) => json!({ "failed": why }),
    }
}

fn json_result(v: &Value) -> Option<Result<Ran, NoRun>> {
    if let Some(ok) = v.get("ok") {
        return Ran::from_json(ok).map(Ok);
    }
    if let Some(why) = v.get("unavailable").and_then(Value::as_str) {
        return Some(Err(NoRun::Unavailable(why.into())));
    }
    if let Some(why) = v.get("refused").and_then(Value::as_str) {
        return Some(Err(NoRun::Refused(why.into())));
    }
    let why = v.get("failed").and_then(Value::as_str)?;
    Some(Err(NoRun::Failed(why.into())))
}

/// A run of [`in_child`]'s, when this binary is run again with
/// [`CHILD_RUN`] set: it prints the run's result as JSON on one line
/// after [`RESULT`]. Run otherwise, it does nothing.
#[test]
fn pair_decode_child() {
    let Some(given) = std::env::var_os(CHILD_RUN) else {
        return;
    };
    let result = given
        .to_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| RunSpec::from_json(&v))
        .ok_or_else(|| {
            NoRun::Failed(format!(
                "{CHILD_RUN} isn't a run: {}",
                given.to_string_lossy()
            ))
        })
        .and_then(|spec| measured(&spec));
    println!("{RESULT}{}", result_json(&result));
}

/// A child process, killed and reaped when dropped, so that a failing
/// test leaves none running.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn read_all<R: Read + Send + 'static>(r: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut r) = r {
            r.read_to_string(&mut s).ok();
        }
        s
    })
}

/// The last `n` lines of `s`, on one line.
fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

/// `spec` run in a child process of its own: this test binary again,
/// running only [`pair_decode_child`].
fn in_child(spec: &RunSpec) -> Result<Ran, NoRun> {
    let exe =
        std::env::current_exe().map_err(|e| NoRun::Failed(format!("this test binary: {e}")))?;
    let mut child = Child(
        Command::new(exe)
            .args(["--exact", "pair_decode_child", "--nocapture"])
            .env(CHILD_RUN, spec.to_json().to_string())
            .env_remove("SPHERECUT_BENCH_OUT")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| NoRun::Failed(format!("starting a child process: {e}")))?,
    );
    let out = read_all(child.0.stdout.take());
    let err = read_all(child.0.stderr.take());
    let began = Instant::now();
    let ended = loop {
        match child.0.try_wait() {
            Ok(Some(status)) => break status.to_string(),
            Ok(None) if began.elapsed() < CHILD_LIMIT => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                child.0.kill().ok();
                break format!("killed after {} s", CHILD_LIMIT.as_secs());
            }
            Err(e) => {
                child.0.kill().ok();
                break format!("not waited for: {e}");
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let line = stdout
        .lines()
        .find_map(|l| l.find(RESULT).map(|at| &l[at + RESULT.len()..]));
    line.and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| json_result(&v))
        .unwrap_or_else(|| {
            Err(NoRun::Failed(format!(
                "the child process ({ended}) gave no result: {}",
                tail(&stderr, 8)
            )))
        })
}

/// A two-track file: its lens streams, the frames the shorter holds as
/// the container counts them, and the format of doctor's checks that
/// covers lens 0's, if one does.
#[derive(Debug, Clone)]
struct Pair {
    path: PathBuf,
    streams: [usize; 2],
    frames: Option<u64>,
    format: Option<&'static str>,
}

/// `path`'s first two video streams, the lenses of a dual-fisheye file.
fn pair_in(path: &Path) -> Result<Pair, String> {
    let report = probe::probe(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let video: Vec<_> = report
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video)
        .collect();
    let [a, b, ..] = video[..] else {
        return Err(format!(
            "{} has {} video streams, not two",
            path.display(),
            video.len()
        ));
    };
    let frames = [a, b]
        .iter()
        .filter_map(|s| s.frames.and_then(|f| u64::try_from(f).ok()))
        .min();
    let format = a
        .codec
        .as_deref()
        .zip(a.video.as_ref())
        .and_then(|(codec, v)| doctor::check_format(codec, v.bit_depth, v.chroma_subsampling));
    Ok(Pair {
        path: path.to_path_buf(),
        streams: [a.index, b.index],
        frames,
        format,
    })
}

/// What a gated clip decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// The 8K30 pair of clip 9: K1 and K4, and K3's runs.
    K1K4,
    /// The synthetic 2×2880² at 29.97: K2's export half.
    K2,
}

/// A clip the bench decodes.
#[derive(Debug, Clone)]
struct Clip {
    /// As the rows name it, such as `X5 8K30 clip 9`.
    name: String,
    file: Result<Pair, String>,
    gate: Option<Gate>,
    /// Where it is coded unlike the X5's mode, as a note beside its rows
    /// says: the 59.94 pair's [`PERF_PAIR_P_ONLY`].
    unlike: Option<&'static str>,
}

/// The bench's clips: clip 9 and the 2×2880² at 29.97 gated, the other
/// modes' samples and perf pairs recorded.
fn bench_clips() -> Vec<Clip> {
    let dir = tier2::samples_dir();
    let sample = |role: &str, gate| {
        let r = samples::entry(role);
        let file = dir
            .as_deref()
            .ok_or_else(|| "no samples folder (SPHERECUT_SAMPLES)".to_string())
            .and_then(|d| samples::master_in(d, r))
            .and_then(|p| pair_in(&p));
        Clip {
            name: format!("X5 {} clip {}", r.mode.name(), r.take),
            file,
            gate,
            unlike: None,
        }
    };
    let perf = |name: &str, gate| Clip {
        name: format!("perf clip {name}"),
        file: tier2::perf_clip(&format!("{name}.mp4")).and_then(|p| pair_in(&p)),
        gate,
        unlike: (name == PERF_PAIR_2880_5994).then_some(PERF_PAIR_P_ONLY),
    };
    vec![
        sample("handheld_flips", Some(Gate::K1K4)),
        perf(PERF_PAIR_2880_2997, Some(Gate::K2)),
        sample("clip2_8k25", None),
        sample("clip3_5k7_60", None),
        sample("clip4_4k120", None),
        perf(PERF_PAIR_3840_2997, None),
        perf(PERF_PAIR_2880_5994, None),
    ]
}

/// A gated row's runs, for the verdicts.
#[derive(Debug, Clone, PartialEq)]
struct GatedRow {
    gate: Gate,
    on: On,
    setup: Setup,
    /// Each run's slower lens, in fps.
    fps: Vec<f64>,
    /// Frames a lens in each run.
    frames: u64,
    beside: bool,
    /// Each run's encode beside, in fps over the decode's timed window
    /// ([`Encoded`]); NaN for a run whose encode took no frame there.
    encode_fps: Vec<f64>,
}

/// A path with no rows at a gated clip, and why.
#[derive(Debug, Clone, PartialEq)]
struct Absent {
    gate: Gate,
    on: On,
    /// The format of doctor's checks that covers the clip, if one does.
    format: Option<&'static str>,
    /// Why its first run has no result: `Unavailable` or `Refused`.
    why: NoRun,
}

/// Doctor's decode checks on the bench's render node: which formats
/// each hardware path decodes, as the playback engine's `HwSupport`
/// reads them for Auto (D34).
#[derive(Debug, Clone, Default)]
struct Doctor(Vec<doctor::Check>);

impl Doctor {
    fn of(report: &DoctorReport) -> Doctor {
        let hardware =
            |c: &&doctor::Check| On::named(&c.path).is_some_and(|on| on.kind().is_some());
        Doctor(
            report
                .decode_checks
                .iter()
                .filter(hardware)
                .cloned()
                .collect(),
        )
    }

    /// `format`, if doctor's check of it works on `on`'s device; else
    /// what doctor says.
    fn decodes(&self, format: Option<&'static str>, on: On) -> Result<&'static str, String> {
        let format = format.ok_or("no check of doctor's covers the clip's format")?;
        match self
            .0
            .iter()
            .find(|c| c.format == format && c.path == on.name())
        {
            None => Err(format!("doctor didn't check {format} on {}", on.label())),
            Some(c) if c.result == CheckResult::Works => Ok(format),
            Some(c) => Err(format!(
                "doctor says {} doesn't decode {format} here: {}",
                on.label(),
                c.reason.as_deref().unwrap_or(match c.result {
                    CheckResult::NotTested => "not tested",
                    _ => "its check fails",
                })
            )),
        }
    }

    /// What doctor says of each hardware path on each of `formats`.
    fn said(&self, formats: &[&'static str]) -> String {
        let mut said = Vec::new();
        for &format in formats {
            for on in On::ALL.into_iter().filter(|on| on.kind().is_some()) {
                said.push(match self.decodes(Some(format), on) {
                    Ok(_) => format!("doctor says {} decodes {format} here", on.label()),
                    Err(says) => says,
                });
            }
        }
        if said.is_empty() {
            return "no clip's format is one doctor checks".into();
        }
        said.join("; ")
    }
}

/// What the gated clips came to, for the verdicts.
#[derive(Debug, Clone, Default)]
struct Gated {
    rows: Vec<GatedRow>,
    absent: Vec<Absent>,
    doctor: Doctor,
}

/// One of K3's runs: the most frames it had alive at once, and how far
/// its peak resident set rose, in KiB, where that can be measured.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grew {
    held: usize,
    kib: Option<u64>,
}

impl From<&Ran> for Grew {
    fn from(r: &Ran) -> Grew {
        Grew {
            held: r.peak_held,
            kib: r.rss_kib,
        }
    }
}

/// K3's runs on one path: lens 0 by itself, and the pair in each setup.
#[derive(Debug, Clone, PartialEq)]
struct K3Row {
    on: On,
    /// Lens 0 by itself, if it ran.
    one: Option<Grew>,
    pairs: Vec<(Setup, Grew)>,
    frame_bytes: usize,
}

/// A K's verdict.
#[derive(Debug, Clone, PartialEq)]
enum Verdict {
    Pass(String),
    /// The redirect, as the plan names it, and why.
    Redirect(&'static str, String),
    NotDecided(String),
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Pass(why) => write!(f, "Pass: {why}"),
            Verdict::Redirect(which, why) => write!(f, "{which}: {why}"),
            Verdict::NotDecided(why) => write!(f, "not decided: {why}"),
        }
    }
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => f64::NAN,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

/// K1's rule on a row's runs: the slower lens's median at least
/// [`PASS_FPS`], and no run under [`RUN_FLOOR_FPS`]. Its median when it
/// keeps it; else why not, and whether that decides anything (a NaN
/// doesn't).
fn k1_rule(fps: &[f64]) -> Result<f64, (String, bool)> {
    if fps.is_empty() {
        return Err(("no runs".into(), false));
    }
    let mut under = Vec::new();
    for &f in fps {
        let floor = at_least(f, RUN_FLOOR_FPS);
        match floor.passes() {
            None => return Err((format!("a run at {f} fps decides nothing"), false)),
            Some(false) => under.push(format!("{f:.1}")),
            Some(true) => {}
        }
    }
    let m = median(fps);
    let held = at_least(m, PASS_FPS);
    if held.passes() != Some(true) {
        return Err((format!("the median {m:.1} fps a lens < {PASS_FPS}"), true));
    }
    if !under.is_empty() {
        return Err((
            format!(
                "the median {m:.1} fps a lens, but a run at {} fps, under {RUN_FLOOR_FPS}",
                under.join(" and ")
            ),
            true,
        ));
    }
    Ok(m)
}

/// How a path's rows at a gate keep K1's rule.
#[derive(Debug, Clone, PartialEq)]
enum Kept {
    /// A setup keeps it: the best, said.
    Yes(String),
    No(String),
    /// A row can't be held to it: short or NaN, or a pass alone or
    /// beside an encode that fell behind.
    Undecided(String),
    /// The path has no row there, and can't decode the clip here: doctor
    /// says it doesn't decode the clip's format, or the device turned the
    /// stream down.
    Cannot(String),
    /// The path has no row there, though nothing says it can't decode the
    /// clip: why.
    Unmeasured(String),
}

/// Each of `fps`, to a tenth.
fn tenths(fps: &[f64]) -> String {
    let said: Vec<String> = fps.iter().map(|f| format!("{f:.1}")).collect();
    said.join(", ")
}

/// Whether `on`'s rows at `gate`, in `setups`, keep K1's rule over
/// `frames` frames a lens. A pass counts only where every run's encode
/// beside held [`ENCODE_FLOOR_FPS`] over the decode's window, since a
/// decode beside a lighter encode, or alone, isn't held to the rule's
/// load. A miss counts whatever the encode took: a lighter load on the
/// shared engine and CPU can only help the decode, so a row that misses
/// beside it would miss beside the full encode too.
fn kept(gated: &Gated, gate: Gate, on: On, setups: &[Setup], frames: u64) -> Kept {
    let mut best: Option<(f64, String)> = None;
    let (mut no, mut undecided) = (Vec::new(), Vec::new());
    let mut any = false;
    for r in gated
        .rows
        .iter()
        .filter(|r| r.gate == gate && r.on == on && setups.contains(&r.setup))
    {
        any = true;
        let label = r.setup.label();
        if r.frames < frames {
            undecided.push(format!(
                "{label} decoded {} frames a lens of {frames}",
                r.frames
            ));
            continue;
        }
        let (runs, encode) = (tenths(&r.fps), tenths(&r.encode_fps));
        let held = r.beside
            && r.encode_fps.len() == r.fps.len()
            && r.encode_fps
                .iter()
                .all(|&e| at_least(e, ENCODE_FLOOR_FPS).passes() == Some(true));
        let load = if held {
            format!("the encode beside at {encode}")
        } else if r.beside {
            format!("even beside an encode at {encode} fps, lighter than K1's load")
        } else {
            "even alone, without K1's encode".to_string()
        };
        match k1_rule(&r.fps) {
            Ok(m) if !held && !r.beside => undecided.push(format!(
                "{label}, {m:.1} fps a lens (the median of {runs}), ran alone, without K1's load"
            )),
            Ok(m) if !held => undecided.push(format!(
                "{label}, {m:.1} fps a lens (the median of {runs}), but the encode beside took \
                 {encode} fps over the decode's window, a run under {ENCODE_FLOOR_FPS}, so the \
                 decode didn't run beside K1's load"
            )),
            Ok(m) if best.as_ref().is_none_or(|(b, _)| m > *b) => {
                let said = format!(
                    "{label}, {m:.1} fps a lens ≥ {PASS_FPS} (the median of {runs}), no run under \
                     {RUN_FLOOR_FPS}, {load}"
                );
                best = Some((m, said));
            }
            Ok(_) => {}
            Err((why, true)) => no.push(format!("{label}: {why} (runs {runs}; {load})")),
            Err((why, false)) => undecided.push(format!("{label}: {why}")),
        }
    }
    match best {
        Some((_, said)) => Kept::Yes(said),
        None if !undecided.is_empty() => Kept::Undecided(undecided.join("; ")),
        None if any => Kept::No(no.join("; ")),
        None => unmeasured(gated, gate, on),
    }
}

/// Why `on` has no rows at `gate`: a path that can't decode the clip
/// here, as doctor or the device says, or one nothing explains.
fn unmeasured(gated: &Gated, gate: Gate, on: On) -> Kept {
    let Some(a) = gated.absent.iter().find(|a| a.gate == gate && a.on == on) else {
        return Kept::Unmeasured("has no rows (see its notes)".into());
    };
    match (&a.why, gated.doctor.decodes(a.format, on)) {
        (NoRun::Refused(why), _) => {
            Kept::Cannot(format!("the device turned the stream down: {why}"))
        }
        (NoRun::Unavailable(why), Err(says)) => Kept::Cannot(format!("{says} ({why})")),
        (NoRun::Unavailable(why), Ok(format)) => Kept::Unmeasured(format!(
            "not measured, though doctor says it decodes {format} here: {why}"
        )),
        (NoRun::Failed(why), _) => Kept::Unmeasured(format!("its runs failed: {why}")),
    }
}

/// VA-API download's verdict with Vulkan Video download's as its first
/// redirect: Pass if VA-API's rows keep K1's rule, else `one` if
/// Vulkan's do, else `two` if Vulkan's miss or Vulkan can't decode the
/// clip here (Redirect 1 holds only where doctor says it decodes HEVC);
/// not decided otherwise.
fn redirects(va: Kept, vk: Kept, one: &'static str, two: &'static str) -> Verdict {
    match va {
        Kept::Yes(why) => Verdict::Pass(format!("VA-API download, {why}")),
        Kept::Undecided(why) | Kept::Cannot(why) | Kept::Unmeasured(why) => {
            Verdict::NotDecided(format!("VA-API download: {why}"))
        }
        Kept::No(va) => match vk {
            Kept::Yes(why) => Verdict::Redirect(
                one,
                format!("VA-API download: {va}; Vulkan Video download, {why}"),
            ),
            Kept::Undecided(why) | Kept::Unmeasured(why) => Verdict::NotDecided(format!(
                "VA-API download: {va}; Vulkan Video download: {why}"
            )),
            Kept::No(vk) | Kept::Cannot(vk) => Verdict::Redirect(
                two,
                format!("VA-API download: {va}; Vulkan Video download: {vk}"),
            ),
        },
    }
}

/// K1, odin's export: VA-API download's best setup at 8K30.
fn k1(gated: &Gated, frames: u64) -> Verdict {
    redirects(
        kept(gated, Gate::K1K4, On::VaApi, &Setup::ALL, frames),
        kept(gated, Gate::K1K4, On::Vulkan, &Setup::ALL, frames),
        REDIRECT_1,
        REDIRECT_2,
    )
}

/// K4, huginn's 8K30 export: VA-API download on a thread a lens.
fn k4(gated: &Gated, frames: u64) -> Verdict {
    redirects(
        kept(gated, Gate::K1K4, On::VaApi, &[Setup::Threaded], frames),
        kept(gated, Gate::K1K4, On::Vulkan, &Setup::ALL, frames),
        REDIRECT_1,
        REDIRECT_HERE,
    )
}

/// K2's export half, huginn's 5.7K30: VA-API download's best setup on
/// the 2×2880² at 29.97; a miss takes K4's redirect.
fn k2(gated: &Gated, frames: u64) -> Verdict {
    redirects(
        kept(gated, Gate::K2, On::VaApi, &Setup::ALL, frames),
        kept(gated, Gate::K2, On::Vulkan, &Setup::ALL, frames),
        REDIRECT_1,
        REDIRECT_HERE,
    )
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

/// `n` frames of `frame_bytes` each, in MiB and MB.
fn frames_size(n: usize, frame_bytes: usize) -> String {
    let bytes = (n * frame_bytes) as f64;
    format!(
        "{:.0} MiB ({:.0} MB)",
        bytes / f64::from(1 << 20),
        bytes / 1e6
    )
}

/// K3 on one path: the pair's growth, in each setup, against
/// [`K3_FACTOR`] × lens 0's by itself. It decides only when each run had
/// alive at once every frame section 12 holds, 10 for the pair and 5
/// for lens 0, since a run that held fewer hides their memory.
fn k3(row: &K3Row) -> Verdict {
    let Some(one) = row.one else {
        return Verdict::NotDecided("lens 0 by itself didn't run".into());
    };
    let Some(one_kib) = one.kib else {
        return Verdict::NotDecided("lens 0 by itself has no growth measured".into());
    };
    let short = |what: &str, held: usize, want: usize| {
        Verdict::NotDecided(format!(
            "{what} had at most {held} frames alive at once, not the {want} section 12 holds"
        ))
    };
    if one.held != HELD_A_LENS {
        return short("lens 0 by itself", one.held, HELD_A_LENS);
    }
    if row.pairs.is_empty() {
        return Verdict::NotDecided("the pair has no run".into());
    }
    let limit = K3_FACTOR * mib(one_kib);
    let mut said = Vec::new();
    let mut over = false;
    for (setup, grew) in &row.pairs {
        let Some(kib) = grew.kib else {
            return Verdict::NotDecided(format!("{} has no growth measured", setup.label()));
        };
        if grew.held != 2 * HELD_A_LENS {
            return short(setup.label(), grew.held, 2 * HELD_A_LENS);
        }
        let held = at_most(mib(kib), limit);
        over |= held.passes() != Some(true);
        said.push(format!("{} {:.0} MiB", setup.label(), mib(kib)));
    }
    let held = row.pairs.iter().map(|(_, g)| g.held).max().unwrap_or(0);
    let why = format!(
        "the pair grew {} against {K3_FACTOR} × lens 0's {:.0} MiB = {limit:.0} MiB; at its peak \
         each pair run had {held} frames alive at once, {} of its growth, and lens 0 by itself \
         had {}, {}",
        said.join(" and "),
        mib(one_kib),
        frames_size(held, row.frame_bytes),
        one.held,
        frames_size(one.held, row.frame_bytes),
    );
    if over {
        Verdict::Redirect(K3_LEVERS, why)
    } else {
        Verdict::Pass(why)
    }
}

/// A recorded line: a name, its runs and their unit.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    name: String,
    values: Vec<f64>,
    unit: &'static str,
}

/// What the bench's runs came to.
#[derive(Debug, Default)]
struct Report {
    rows: Vec<Row>,
    notes: Vec<String>,
    /// The runs that failed, other than for a path the machine lacks.
    failed: Vec<String>,
    /// Each K's verdict on this machine, by a short name.
    verdicts: Vec<(String, Verdict)>,
    /// Whether each row and note is recorded through `bench` as it is
    /// added.
    echo: bool,
}

impl Report {
    fn row(&mut self, name: String, values: Vec<f64>, unit: &'static str) {
        if self.echo {
            bench::record_runs(&name, &values, unit);
        }
        self.rows.push(Row { name, values, unit });
    }

    fn note(&mut self, text: String) {
        if self.echo {
            bench::note(&text);
        }
        self.notes.push(text);
    }

    /// `rans`' rows, under `name`.
    fn add(&mut self, name: &str, spec: &RunSpec, rans: &[Ran]) {
        let alone = if spec.beside.is_some() {
            " beside the encode"
        } else {
            " alone"
        };
        let lens = if spec.streams.len() == 1 {
            "lens 0"
        } else {
            "slower lens"
        };
        let each = |f: &dyn Fn(&Ran) -> f64| rans.iter().map(f).collect::<Vec<f64>>();
        self.row(
            format!("{name}: {lens}{alone}"),
            each(&Ran::slower_fps),
            "fps",
        );
        self.row(format!("{name}: opening"), each(&|r| r.open_s * 1e3), "ms");
        if spec.beside.is_some() {
            let fps = each(&|r| r.encode.map_or(f64::NAN, Encoded::fps));
            self.row(
                format!("{name}: the encode beside, in the decode's window"),
                fps,
                "fps",
            );
        }
        let rss: Option<Vec<f64>> = rans.iter().map(|r| r.rss_kib.map(mib)).collect();
        match rss {
            Some(rss) => self.row(format!("{name}: peak RSS growth{alone}"), rss, "MiB"),
            None => self.note(format!(
                "{name}: peak RSS growth: not measured: {CLEAR_REFS} can't reset the peak here"
            )),
        }
    }

    /// The Ks this machine left undecided, by their short names.
    fn undecided(&self) -> Vec<&str> {
        self.verdicts
            .iter()
            .filter(|(_, v)| matches!(v, Verdict::NotDecided(_)))
            .map(|(k, _)| k.as_str())
            .collect()
    }

    /// Fails the test on a run that failed, and skips it when no K was
    /// decided, naming them.
    fn end(&self) {
        assert!(
            self.failed.is_empty(),
            "{} run(s) failed:\n{}",
            self.failed.len(),
            self.failed.join("\n")
        );
        let undecided = self.undecided();
        if undecided.len() == self.verdicts.len() {
            tier2::skip(format!(
                "no K was decided on this machine ({} undecided; see their notes); the rows \
                 measured are recorded",
                undecided.join(", ")
            ));
        }
    }
}

/// What the bench runs.
#[derive(Debug)]
struct Plan {
    clips: Vec<Clip>,
    paths: Vec<On>,
    node: Option<PathBuf>,
    /// The encode beside the gated rows, or why it can't run.
    beside: Result<Beside, String>,
    scratch: PathBuf,
    /// Frames a lens in a gated run, and the most in any.
    frames: u64,
    /// A gated row's runs after its warm-up.
    gated_runs: usize,
    /// Doctor's decode checks on `node`, which say where a path missing
    /// at a gate could decode it (K1's Redirect 1).
    doctor: Doctor,
    echo: bool,
}

impl Plan {
    fn spec(&self, pair: &Pair, streams: &[usize], on: On, setup: Setup) -> RunSpec {
        RunSpec {
            file: pair.path.clone(),
            streams: streams.to_vec(),
            on,
            setup,
            frames: pair.frames.map_or(self.frames, |f| f.min(self.frames)),
            ahead: DECODED_AHEAD,
            fill: false,
            node: self.node.clone(),
            beside: None,
            scratch: self.scratch.clone(),
        }
    }

    /// `spec` run `runs` times on `clip`, each in a child process of its
    /// own, the first to warm up when there are several; its rows go
    /// under `name`. Each run that decodes goes into `measured`, which
    /// judges a run that finds the path missing ([`Measured::judge`]).
    fn measure(
        &self,
        (clip, name): (&str, &str),
        spec: &RunSpec,
        runs: usize,
        measured: &mut Measured,
        report: &mut Report,
    ) -> Result<Vec<Ran>, NoRun> {
        let mut rans = Vec::with_capacity(runs);
        for _ in 0..runs {
            match in_child(spec) {
                Ok(ran) => {
                    measured.add(spec.on, clip, name);
                    rans.push(ran);
                }
                Err(no) => return Err(measured.judge(spec.on, clip, no)),
            }
        }
        if runs > 1 {
            rans.remove(0);
        }
        let most = HELD_A_LENS * spec.streams.len();
        if let Some(r) = rans
            .iter()
            .find(|r| at_most(r.peak_held, most).passes() != Some(true))
        {
            return Err(NoRun::Failed(format!(
                "{} frames were alive at once, more than section 12's {most}",
                r.peak_held
            )));
        }
        report.add(name, spec, &rans);
        Ok(rans)
    }

    fn run(&self) -> Report {
        let mut report = Report {
            echo: self.echo,
            ..Report::default()
        };
        if let Err(why) = &self.beside {
            report.note(format!(
                "pair decode: the gated rows run alone, as the encode beside can't: {why}"
            ));
        }
        let mut formats: Vec<&'static str> = Vec::new();
        for f in self
            .clips
            .iter()
            .filter_map(|c| c.file.as_ref().ok()?.format)
        {
            if !formats.contains(&f) {
                formats.push(f);
            }
        }
        report.note(format!("pair decode: {}", self.doctor.said(&formats)));
        let mut gated = Gated {
            doctor: self.doctor.clone(),
            ..Gated::default()
        };
        let mut k3s = Vec::new();
        let mut measured = Measured::default();
        for clip in &self.clips {
            let pair = match &clip.file {
                Ok(pair) => pair,
                Err(why) => {
                    report.note(format!("pair decode {}: not measured: {why}", clip.name));
                    continue;
                }
            };
            if let Some(unlike) = clip.unlike {
                report.note(format!("pair decode {}: {unlike}", clip.name));
            }
            let frames = pair.frames.map_or(self.frames, |f| f.min(self.frames));
            if frames < self.frames {
                report.note(format!(
                    "pair decode {}: {frames} frames a lens, all the clip holds",
                    clip.name
                ));
            }
            let beside = clip.gate.and(self.beside.as_ref().ok().copied());
            let runs = if clip.gate.is_some() {
                1 + self.gated_runs
            } else {
                1
            };
            for &on in &self.paths {
                let name =
                    |setup: &str| format!("pair decode {}, {}, {setup}", clip.name, on.label());
                let mut unavailable = None;
                for setup in Setup::ALL {
                    let spec = RunSpec {
                        beside,
                        ..self.spec(pair, &pair.streams, on, setup)
                    };
                    let named = (clip.name.as_str(), name(setup.label()));
                    let ran =
                        self.measure((named.0, &named.1), &spec, runs, &mut measured, &mut report);
                    match ran {
                        Ok(rans) => {
                            if let Some(gate) = clip.gate {
                                let encode_fps = match beside {
                                    Some(_) => rans
                                        .iter()
                                        .map(|r| r.encode.map_or(f64::NAN, Encoded::fps))
                                        .collect(),
                                    None => Vec::new(),
                                };
                                gated.rows.push(GatedRow {
                                    gate,
                                    on,
                                    setup,
                                    fps: rans.iter().map(Ran::slower_fps).collect(),
                                    frames,
                                    beside: beside.is_some(),
                                    encode_fps,
                                });
                            }
                        }
                        Err(no @ (NoRun::Unavailable(_) | NoRun::Refused(_))) => {
                            unavailable = Some(no);
                            break;
                        }
                        Err(NoRun::Failed(why)) => {
                            report.failed.push(format!("{}: {why}", named.1));
                        }
                    }
                }
                if let Some(no) = unavailable {
                    report.note(format!(
                        "pair decode {}, {}: not measured: {}",
                        clip.name,
                        on.label(),
                        no.why()
                    ));
                    if let Some(gate) = clip.gate {
                        gated.absent.push(Absent {
                            gate,
                            on,
                            format: pair.format,
                            why: no,
                        });
                    }
                    continue;
                }
                if clip.gate == Some(Gate::K1K4) {
                    k3s.push(self.k3_runs(clip, pair, on, &mut measured, &mut report));
                }
            }
        }
        self.verdicts(&gated, &k3s, &mut report);
        report
    }

    /// K3's runs on `on`, alone: lens 0 by itself, then the pair in each
    /// setup, each with every channel kept full.
    fn k3_runs(
        &self,
        clip: &Clip,
        pair: &Pair,
        on: On,
        measured: &mut Measured,
        report: &mut Report,
    ) -> K3Row {
        let mut row = K3Row {
            on,
            one: None,
            pairs: Vec::new(),
            frame_bytes: 0,
        };
        let name =
            |setup: &str| format!("pair decode {}, {}, {setup}, for K3", clip.name, on.label());
        let spec = |streams: &[usize], setup| RunSpec {
            fill: true,
            ..self.spec(pair, streams, on, setup)
        };
        let one = spec(&pair.streams[..1], Setup::Threaded);
        let mut runs = vec![(None, name("lens 0 only"), one)];
        for setup in Setup::ALL {
            runs.push((Some(setup), name(setup.label()), spec(&pair.streams, setup)));
        }
        for (setup, name, spec) in runs {
            match self.measure((&clip.name, &name), &spec, 1, measured, report) {
                Ok(rans) => match setup {
                    None => {
                        row.one = Some(Grew::from(&rans[0]));
                        row.frame_bytes = rans[0].frame_bytes;
                    }
                    Some(setup) => row.pairs.push((setup, Grew::from(&rans[0]))),
                },
                Err(NoRun::Unavailable(why) | NoRun::Refused(why)) => {
                    report.note(format!("{name}: not measured: {why}"));
                }
                Err(NoRun::Failed(why)) => report.failed.push(format!("{name}: {why}")),
            }
        }
        row
    }

    /// Each K's verdict on this machine, as a note, under the plan's name
    /// for the machine it was written for; then the Ks left undecided.
    fn verdicts(&self, gated: &Gated, k3s: &[K3Row], report: &mut Report) {
        let clip = |gate| {
            self.clips
                .iter()
                .find(|c| c.gate == Some(gate))
                .map(|c| (c.name.clone(), c.file.as_ref().err().cloned()))
        };
        let missing = |gate| match clip(gate) {
            Some((name, Some(why))) => {
                Some(Verdict::NotDecided(format!("{name} isn't here: {why}")))
            }
            None => Some(Verdict::NotDecided("no clip for it".into())),
            Some((_, None)) => None,
        };
        let named = |gate| clip(gate).map_or_else(String::new, |(name, _)| name);
        let (at_8k, at_5k7) = (named(Gate::K1K4), named(Gate::K2));
        let on_8k = missing(Gate::K1K4);
        let first = on_8k.clone().unwrap_or_else(|| k1(gated, self.frames));
        let fourth = on_8k.unwrap_or_else(|| k4(gated, self.frames));
        let second = missing(Gate::K2).unwrap_or_else(|| k2(gated, self.frames));
        let mut verdicts = vec![(
            "K1".to_string(),
            format!("K1's rule (odin's export), on this machine, {at_8k} beside the encode"),
            first,
        )];
        verdicts.push((
            "K2's export half".into(),
            format!(
                "K2's rule for its export half (huginn's 5.7K30; its engine half is \
                 bench_playback_x5_raw's), on this machine, {at_5k7} beside the encode"
            ),
            second,
        ));
        if k3s.is_empty() {
            let why = missing(Gate::K1K4)
                .unwrap_or_else(|| Verdict::NotDecided("no path decoded it".into()));
            verdicts.push((
                "K3".into(),
                format!("K3 (memory), on this machine, {at_8k} alone"),
                why,
            ));
        }
        for row in k3s {
            verdicts.push((
                format!("K3 on {}", row.on.label()),
                format!(
                    "K3 (memory) on {}, on this machine, {at_8k} alone",
                    row.on.label()
                ),
                k3(row),
            ));
        }
        verdicts.push((
            "K4".into(),
            format!("K4's rule (huginn's 8K30 export), on this machine, {at_8k} beside the encode"),
            fourth,
        ));
        for (short, said, verdict) in verdicts {
            report.note(format!("{said}: {verdict}"));
            report.verdicts.push((short, verdict));
        }
        let undecided = report.undecided().join(", ");
        if !undecided.is_empty() {
            report.note(format!("left undecided on this machine: {undecided}"));
        }
    }
}

#[test]
#[ignore = "tier2: benchmark, needs a GPU with VA-API, the X5 samples and hw-test's perf clips"]
fn bench_export_pair_decode() {
    let node = tier2::require_render_node();
    let clips = bench_clips();
    if clips.iter().all(|c| c.file.is_err()) {
        let why: Vec<String> = clips
            .iter()
            .filter_map(|c| c.file.as_ref().err().map(|w| format!("{}: {w}", c.name)))
            .collect();
        tier2::skip(format!("no clip to decode: {}", why.join("; ")));
    }
    let scratch = Scratch::new("pair-decode");
    let beside = Beside {
        kind: EncoderKind::Vaapi,
        size: ENCODE_SIZE,
    };
    let setup = beside.setup();
    bench::note(format!(
        "pair decode: devices on {}; the encode beside is {} at {}×{}, 30 fps, {} Mbit/s",
        node.display(),
        setup.name,
        ENCODE_SIZE.0,
        ENCODE_SIZE.1,
        setup.bitrate / 1_000_000
    ));
    // Doctor's decode checks, as the playback engine's Auto reads them,
    // without asking the package manager.
    let checks = doctor::run_with(
        &node,
        doctor::Options {
            query_packages: false,
        },
    );
    let plan = Plan {
        clips,
        paths: On::ALL.to_vec(),
        beside: probe_beside(beside, Some(&node), &scratch.0),
        node: Some(node),
        scratch: scratch.0.clone(),
        frames: FRAMES,
        gated_runs: GATED_RUNS,
        doctor: Doctor::of(&checks),
        echo: true,
    };
    plan.run().end();
}

/// Section 9's 2×64² pair called `name`, written once.
fn pair_64(name: &str) -> Pair {
    static WRITTEN: OnceLock<Vec<PathBuf>> = OnceLock::new();
    let files = WRITTEN.get_or_init(|| {
        let dir = media_dir().join("pairs-64");
        std::fs::create_dir_all(&dir).unwrap();
        synth::pairs_64()
            .iter()
            .map(|p| {
                let path = dir.join(p.file_name());
                if path.exists() {
                    path
                } else {
                    synth::write_pair(p, &dir).unwrap()
                }
            })
            .collect()
    });
    let path = files
        .iter()
        .find(|p| p.file_stem().is_some_and(|s| s == name))
        .unwrap();
    pair_in(path).unwrap()
}

/// A Tier 1 run of `name`'s pair in software, all of it.
fn spec_64(name: &str, setup: Setup) -> RunSpec {
    let pair = pair_64(name);
    RunSpec {
        streams: pair.streams.to_vec(),
        frames: pair.frames.unwrap(),
        file: pair.path,
        on: On::Software,
        setup,
        ahead: DECODED_AHEAD,
        fill: false,
        node: None,
        beside: None,
        scratch: media_dir(),
    }
}

#[test]
fn the_pair_decode_holds_the_frames_section_12_holds() {
    let _time = timed("the_pair_decode_holds_the_frames_section_12_holds");
    for setup in Setup::ALL {
        // K3's consumer, which lets a pair go only once every channel is
        // full.
        let spec = RunSpec {
            fill: true,
            ..spec_64(PAIR_64_MATCHED, setup)
        };
        let ran = run(&spec).unwrap();
        let frames: Vec<u64> = ran.lenses.iter().map(|l| l.frames).collect();
        assert_eq!(frames, [30, 30], "{setup:?}");
        assert!(ran.slower_fps() > 0.0, "{setup:?}: {ran:?}");
        assert_eq!(ran.peak_held, 2 * HELD_A_LENS, "{setup:?}");
        // The control: a channel a frame deeper holds a frame more a
        // lens.
        let deeper = run(&RunSpec {
            ahead: DECODED_AHEAD + 1,
            ..spec.clone()
        })
        .unwrap();
        must_fail("a channel of 3", deeper.peak_held == 2 * HELD_A_LENS);
        assert_eq!(deeper.peak_held, 2 * (HELD_A_LENS + 1), "{setup:?}");
        // Lens 0 by itself, K3's baseline, holds half.
        let one = run(&RunSpec {
            streams: spec.streams[..1].to_vec(),
            ..spec
        })
        .unwrap();
        assert_eq!(one.lenses.len(), 1);
        assert_eq!(one.peak_held, HELD_A_LENS, "{setup:?}");
    }
}

#[test]
fn the_zip_holds_the_lenses_to_one_time_and_the_path_to_its_name() {
    let _time = timed("the_zip_holds_the_lenses_to_one_time_and_the_path_to_its_name");
    let pair = pair_64(PAIR_64_MATCHED);
    let live = Arc::new(Live::default());
    let backend = FfmpegBackend::new();
    let open = |k: usize| {
        let req = VideoRequest {
            stream: Some(pair.streams[k]),
            ..VideoRequest::default()
        };
        backend.open_video(&pair.path, &req).unwrap()
    };
    let frame = |dec: &mut Box<dyn VideoDecode>| match dec.next_frame(&StopToken::never()) {
        Ok(NextFrame::Frame(f)) => Held::new(f, &live),
        other => panic!("no frame: {other:?}"),
    };
    let (mut a, mut b) = (open(0), open(1));
    let both = [frame(&mut a), frame(&mut b)];
    same_time(0, &both).unwrap();
    let [a0, _b0] = both;
    let apart = must_fail(
        "lens 0's frames 0 and 1 zipped",
        same_time(0, &[a0, frame(&mut a)]),
    );
    assert!(apart.contains("apart"), "{apart}");
    assert_eq!(live.peak.load(Ordering::SeqCst), 3);
    // A software frame is software's, and not a device's: the control.
    let path = DecodePath::Software { threads: None };
    assert!(On::Software.decodes_on(&path));
    must_fail(
        "a software frame taken for VA-API's",
        On::VaApi.decodes_on(&path),
    );
}

#[test]
fn a_pair_decodes_as_far_as_its_shorter_lens() {
    let _time = timed("a_pair_decodes_as_far_as_its_shorter_lens");
    let spec = spec_64(PAIR_64_LENS_SHORT, Setup::Threaded);
    // The container's count, the shorter lens's.
    assert_eq!(spec.frames, 29);
    let ran = run(&spec).unwrap();
    let frames: Vec<u64> = ran.lenses.iter().map(|l| l.frames).collect();
    assert_eq!(frames, [29, 29]);
    // The control: the longer lens's count fails the run at the shorter
    // lens's end, and K3's consumer, waiting for the channels to fill,
    // lets the run end too.
    for setup in Setup::ALL {
        for fill in [false, true] {
            let longer = RunSpec {
                frames: 30,
                setup,
                fill,
                ..spec.clone()
            };
            let why = must_fail("the short lens asked for 30 frames", run(&longer));
            assert_eq!(
                why,
                NoRun::Failed("lens 1 ended after 29 frames of 30".into()),
                "{setup:?}, fill {fill}"
            );
        }
    }
}

#[test]
fn k1s_rule_and_k3s_hold_numbers_to_the_plans_limits() {
    let _time = timed("k1s_rule_and_k3s_hold_numbers_to_the_plans_limits");
    assert_eq!(k1_rule(&[40.0, 42.0, 41.0]), Ok(41.0));
    assert_eq!(k1_rule(&[36.0, 30.0, 36.0]), Ok(36.0));
    let (why, decided) = must_fail("a run under 30", k1_rule(&[40.0, 41.0, 29.0]));
    assert!(decided && why.contains("29.0"), "{why}");
    let (why, decided) = must_fail("a median under 36", k1_rule(&[35.0, 35.5, 37.0]));
    assert!(decided && why.contains("35.5"), "{why}");
    let (_, decided) = must_fail("a NaN run", k1_rule(&[40.0, f64::NAN, 41.0]));
    assert!(!decided);
    let (_, decided) = must_fail("no runs", k1_rule(&[]));
    assert!(!decided);
    let full = |mib: u64, held| Grew {
        held,
        kib: Some(mib << 10),
    };
    let row = |pairs: [u64; 2]| K3Row {
        on: On::VaApi,
        one: Some(full(400, HELD_A_LENS)),
        pairs: vec![
            (Setup::Serial, full(pairs[0], 2 * HELD_A_LENS)),
            (Setup::Threaded, full(pairs[1], 2 * HELD_A_LENS)),
        ],
        frame_bytes: 3840 * 3840 * 3 / 2,
    };
    // 2 × 400 MiB + 10 % = 880 MiB.
    let pass = k3(&row([800, 880]));
    assert!(matches!(pass, Verdict::Pass(_)), "{pass}");
    // Section 12's 10 frames at 8K: 221 MB, and lens 0's 5 half that.
    let said = pass.to_string();
    assert!(
        said.contains("10 frames alive at once, 211 MiB (221 MB)"),
        "{said}"
    );
    assert!(said.contains("had 5, 105 MiB (111 MB)"), "{said}");
    let over = k3(&row([800, 920]));
    must_fail("a pair at 2.3 × lens 0", matches!(over, Verdict::Pass(_)));
    assert!(matches!(over, Verdict::Redirect(K3_LEVERS, _)), "{over}");
    let unmeasured = K3Row {
        one: Some(Grew {
            held: HELD_A_LENS,
            kib: None,
        }),
        ..row([800, 800])
    };
    assert!(matches!(k3(&unmeasured), Verdict::NotDecided(_)));
    // A run that never had all its frames alive at once decides nothing,
    // even within the limit: the controls for the pass above.
    let mut short = row([800, 800]);
    short.pairs[1].1.held = 2 * HELD_A_LENS - 1;
    let says = k3(&short);
    must_fail(
        "a pair run that held 9 frames",
        matches!(says, Verdict::Pass(_)),
    );
    assert!(
        says.to_string()
            .contains("a thread a lens had at most 9 frames alive at once, not the 10"),
        "{says}"
    );
    let mut short = row([800, 800]);
    short.one = Some(full(400, HELD_A_LENS - 3));
    let says = k3(&short);
    must_fail(
        "lens 0 by itself held 2 frames",
        matches!(says, Verdict::Pass(_)),
    );
    assert!(matches!(says, Verdict::NotDecided(_)), "{says}");
}

#[test]
fn each_k_names_its_verdict_by_the_plans_rules() {
    let _time = timed("each_k_names_its_verdict_by_the_plans_rules");
    let row = |gate, on, setup, fps: [f64; 3]| GatedRow {
        gate,
        on,
        setup,
        fps: fps.to_vec(),
        frames: FRAMES,
        beside: true,
        encode_fps: vec![30.0; 3],
    };
    let fast = [40.0, 41.0, 42.0];
    let slow = [25.0, 26.0, 27.0];
    let rows = |rows: Vec<GatedRow>| Gated {
        rows,
        ..Gated::default()
    };
    let at_8k = |va: [[f64; 3]; 2], vk: [f64; 3]| {
        rows(vec![
            row(Gate::K1K4, On::VaApi, Setup::Serial, va[0]),
            row(Gate::K1K4, On::VaApi, Setup::Threaded, va[1]),
            row(Gate::K1K4, On::Vulkan, Setup::Threaded, vk),
        ])
    };
    // VA-API's best setup keeps the rule: Pass, whichever it is.
    let serial_only = at_8k([fast, slow], slow);
    assert!(matches!(k1(&serial_only, FRAMES), Verdict::Pass(_)));
    // K4 counts a thread a lens only: the control.
    let k4_says = k4(&serial_only, FRAMES);
    must_fail(
        "K4 passed on the serial setup",
        matches!(k4_says, Verdict::Pass(_)),
    );
    assert!(
        matches!(k4_says, Verdict::Redirect(REDIRECT_HERE, _)),
        "{k4_says}"
    );
    // VA-API misses and Vulkan keeps it: Redirect 1, as odin expects.
    let odin = at_8k([slow, slow], fast);
    let says = k1(&odin, FRAMES);
    assert!(matches!(says, Verdict::Redirect(REDIRECT_1, _)), "{says}");
    assert!(matches!(
        k4(&odin, FRAMES),
        Verdict::Redirect(REDIRECT_1, _)
    ));
    // The encode's rate is in the verdict.
    assert!(
        says.to_string()
            .contains("the encode beside at 30.0, 30.0, 30.0"),
        "{says}"
    );
    // Both miss: Redirect 2.
    let neither = at_8k([slow, slow], slow);
    assert!(matches!(
        k1(&neither, FRAMES),
        Verdict::Redirect(REDIRECT_2, _)
    ));
    // VA-API missing, or short, decides nothing; so does Vulkan's pass
    // alone, beside VA-API's miss alone.
    assert!(matches!(
        k1(&rows(odin.rows[2..].to_vec()), FRAMES),
        Verdict::NotDecided(_)
    ));
    let changed = |g: &Gated, f: &dyn Fn(&mut GatedRow)| {
        let mut g = g.clone();
        g.rows.iter_mut().for_each(f);
        g
    };
    let alone = |r: &mut GatedRow| {
        r.beside = false;
        r.encode_fps.clear();
    };
    let says = k1(&changed(&odin, &alone), FRAMES);
    assert!(matches!(says, Verdict::NotDecided(_)), "{says}");
    assert!(
        says.to_string().contains(
            "a thread a lens, 41.0 fps a lens (the median of 40.0, 41.0, 42.0), ran alone"
        ),
        "{says}"
    );
    let short = changed(&odin, &|r| r.frames = FRAMES - 1);
    assert!(matches!(k1(&short, FRAMES), Verdict::NotDecided(_)));
    // An encode beside that held 20 fps over the decode's window isn't
    // K1's load: rows that would pass beside it decide nothing, on K1,
    // K2 and K4 alike. The control is the same rows at 30 fps, which
    // pass.
    let all_fast = |gate| {
        rows(vec![
            row(gate, On::VaApi, Setup::Serial, fast),
            row(gate, On::VaApi, Setup::Threaded, fast),
            row(gate, On::Vulkan, Setup::Threaded, fast),
        ])
    };
    let at_20 = |mut g: Gated| {
        g.rows.iter_mut().for_each(|r| r.encode_fps[1] = 20.0);
        g
    };
    for (k, gate, verdict) in [
        ("K1", Gate::K1K4, k1 as fn(&Gated, u64) -> Verdict),
        ("K2", Gate::K2, k2),
        ("K4", Gate::K1K4, k4),
    ] {
        assert!(
            matches!(verdict(&all_fast(gate), FRAMES), Verdict::Pass(_)),
            "{k}"
        );
        let says = verdict(&at_20(all_fast(gate)), FRAMES);
        must_fail(
            &format!("{k} kept by rows whose encode beside held 20 fps"),
            matches!(says, Verdict::Pass(_)),
        );
        assert!(matches!(says, Verdict::NotDecided(_)), "{k}: {says}");
        assert!(
            says.to_string()
                .contains("took 30.0, 20.0, 30.0 fps over the decode's window"),
            "{k}: {says}"
        );
    }
    // A miss beside a 20 fps encode, or alone, is a miss: the full
    // encode could only slow the decode further. Both paths missing so
    // take Redirect 2 for K1 and huginn's redirect for K4, and the miss
    // says what it ran beside.
    for (k, verdict, redirect) in [
        ("K1", k1 as fn(&Gated, u64) -> Verdict, REDIRECT_2),
        ("K4", k4, REDIRECT_HERE),
    ] {
        let says = verdict(&at_20(neither.clone()), FRAMES);
        must_fail(
            &format!("{k}'s miss beside a 20 fps encode left undecided"),
            matches!(says, Verdict::NotDecided(_)),
        );
        assert!(
            matches!(says, Verdict::Redirect(r, _) if r == redirect),
            "{k}: {says}"
        );
        assert!(
            says.to_string().contains(
                "a thread a lens: the median 26.0 fps a lens < 36 (runs 25.0, 26.0, 27.0; even \
                 beside an encode at 30.0, 20.0, 30.0 fps, lighter than K1's load)"
            ),
            "{k}: {says}"
        );
        let says = verdict(&changed(&neither, &alone), FRAMES);
        must_fail(
            &format!("{k}'s miss alone left undecided"),
            matches!(says, Verdict::NotDecided(_)),
        );
        assert!(
            matches!(says, Verdict::Redirect(r, _) if r == redirect),
            "{k}: {says}"
        );
        assert!(
            says.to_string().contains("even alone, without K1's encode"),
            "{k}: {says}"
        );
    }
    // A pass beside a slow encode beside another setup's miss leaves
    // the path undecided: the pass might hold under the full load.
    let mixed = at_8k([fast, slow], slow);
    let mut mixed_slow = mixed.clone();
    mixed_slow.rows[0].encode_fps[1] = 20.0;
    assert!(matches!(k1(&mixed, FRAMES), Verdict::Pass(_)));
    let says = k1(&mixed_slow, FRAMES);
    must_fail(
        "a pass beside a 20 fps encode outranked by a miss",
        matches!(says, Verdict::Redirect(..)),
    );
    assert!(matches!(says, Verdict::NotDecided(_)), "{says}");
    // K2 reads the 2×2880² rows only.
    assert!(matches!(k2(&odin, FRAMES), Verdict::NotDecided(_)));
    let huginn = rows(vec![row(Gate::K2, On::VaApi, Setup::Threaded, fast)]);
    assert!(matches!(k2(&huginn, FRAMES), Verdict::Pass(_)));
    // huginn's VA-API misses, and its doctor says Vulkan Video doesn't
    // decode HEVC there: K4's redirect.
    let missed = Gated {
        absent: vec![Absent {
            gate: Gate::K2,
            on: On::Vulkan,
            format: Some("HEVC Main"),
            why: NoRun::Unavailable("Vulkan device unavailable".into()),
        }],
        doctor: doctor_says(CheckResult::Works, CheckResult::Fails),
        ..rows(vec![row(Gate::K2, On::VaApi, Setup::Threaded, slow)])
    };
    assert!(matches!(
        k2(&missed, FRAMES),
        Verdict::Redirect(REDIRECT_HERE, _)
    ));
}

#[test]
fn the_bench_skips_only_when_no_k_is_decided() {
    let _time = timed("the_bench_skips_only_when_no_k_is_decided");
    let report = |verdicts: [(&str, Verdict); 3]| Report {
        verdicts: verdicts.map(|(k, v)| (k.to_string(), v)).to_vec(),
        ..Report::default()
    };
    let undecided = || Verdict::NotDecided("no rows".into());
    let k3 = || Verdict::Pass("the pair grew 2.0 × lens 0".into());
    // K3 in software decided, with no hardware row: the bench ends.
    let some = report([
        ("K1", undecided()),
        ("K3 on software", k3()),
        ("K4", undecided()),
    ]);
    assert_eq!(some.undecided(), ["K1", "K4"]);
    some.end();
    // The control: with none decided it skips, naming each.
    let none = report([
        ("K1", undecided()),
        ("K3 on software", undecided()),
        ("K4", undecided()),
    ]);
    let skipped = must_fail(
        "the end of a run that decided no K",
        std::panic::catch_unwind(move || none.end()),
    );
    let why = skipped.downcast_ref::<String>().unwrap();
    assert!(
        why.starts_with(tier2::SKIP_MARKER)
            && why.contains("no K was decided on this machine (K1, K3 on software, K4 undecided"),
        "{why}"
    );
}

/// Doctor's checks of HEVC Main on VA-API and Vulkan Video.
fn doctor_says(vaapi: CheckResult, vulkan: CheckResult) -> Doctor {
    let check = |path: &str, result| doctor::Check {
        format: "HEVC Main".into(),
        path: path.into(),
        codec: "hevc".into(),
        result,
        reason: (result != CheckResult::Works).then(|| format!("{path} said no")),
        fault: None,
        ffmpeg_log: Vec::new(),
    };
    Doctor(vec![check("vaapi", vaapi), check("vulkan", vulkan)])
}

#[test]
fn a_missing_vulkan_redirects_only_where_doctor_or_the_device_says_it_cant() {
    let _time = timed("a_missing_vulkan_redirects_only_where_doctor_or_the_device_says_it_cant");
    let slow = |setup| GatedRow {
        gate: Gate::K1K4,
        on: On::VaApi,
        setup,
        fps: vec![25.0, 26.0, 27.0],
        frames: FRAMES,
        beside: true,
        encode_fps: vec![30.0; 3],
    };
    // VA-API misses and Vulkan Video's open failed.
    let gated = |doctor, why| Gated {
        rows: vec![slow(Setup::Serial), slow(Setup::Threaded)],
        absent: vec![Absent {
            gate: Gate::K1K4,
            on: On::Vulkan,
            format: Some("HEVC Main"),
            why,
        }],
        doctor,
    };
    let failed_open = || NoRun::Unavailable("Vulkan device unavailable: it went".into());
    let works = doctor_says(CheckResult::Works, CheckResult::Works);
    // Doctor says Vulkan Video decodes HEVC here, where Redirect 1 would
    // send the pairs: its missing rows decide nothing.
    let unmeasured = gated(works.clone(), failed_open());
    let says = k1(&unmeasured, FRAMES);
    assert!(matches!(says, Verdict::NotDecided(_)), "{says}");
    assert!(
        says.to_string()
            .contains("though doctor says it decodes HEVC Main here"),
        "{says}"
    );
    let fourth = k4(&unmeasured, FRAMES);
    assert!(matches!(fourth, Verdict::NotDecided(_)), "{fourth}");
    // The controls: where doctor says Vulkan Video doesn't decode HEVC,
    // or the device turned the stream down, Redirect 1 can't hold and
    // the miss goes past it.
    let cannot = gated(
        doctor_says(CheckResult::Works, CheckResult::Fails),
        failed_open(),
    );
    let says = k1(&cannot, FRAMES);
    must_fail(
        "doctor saying Vulkan Video can't decode HEVC Main",
        matches!(says, Verdict::NotDecided(_)),
    );
    assert!(matches!(says, Verdict::Redirect(REDIRECT_2, _)), "{says}");
    assert!(says.to_string().contains("vulkan said no"), "{says}");
    assert!(matches!(
        k4(&cannot, FRAMES),
        Verdict::Redirect(REDIRECT_HERE, _)
    ));
    let not_tested = gated(
        doctor_says(CheckResult::Works, CheckResult::NotTested),
        failed_open(),
    );
    assert!(matches!(
        k1(&not_tested, FRAMES),
        Verdict::Redirect(REDIRECT_2, _)
    ));
    let refused = gated(
        works.clone(),
        NoRun::Refused("cannot decode it on Vulkan: too large".into()),
    );
    let says = k1(&refused, FRAMES);
    must_fail(
        "the device turning the stream down",
        matches!(says, Verdict::NotDecided(_)),
    );
    assert!(matches!(says, Verdict::Redirect(REDIRECT_2, _)), "{says}");
    // A clip no check of doctor's covers: doctor can't say it decodes it.
    let uncovered = Gated {
        absent: vec![Absent {
            format: None,
            ..unmeasured.absent[0].clone()
        }],
        ..unmeasured.clone()
    };
    assert!(matches!(
        k1(&uncovered, FRAMES),
        Verdict::Redirect(REDIRECT_2, _)
    ));
}

#[test]
fn a_path_that_decoded_here_cant_go_missing_later() {
    let _time = timed("a_path_that_decoded_here_cant_go_missing_later");
    let gone = || NoRun::Unavailable("VA-API device unavailable: it went".into());
    let refused = || NoRun::Refused("cannot decode it on VA-API: too large".into());
    let mut measured = Measured::default();
    // Before any run decoded, a missing path is a note.
    assert_eq!(measured.judge(On::VaApi, "clip 9", gone()), gone());
    assert_eq!(measured.judge(On::VaApi, "clip 9", refused()), refused());
    measured.add(
        On::VaApi,
        "clip 9",
        "pair decode clip 9, VA-API download, serial",
    );
    // Once serial decoded, a thread a lens, a later child run or a K3 run
    // that finds the device missing fails.
    let after = measured.judge(On::VaApi, "clip 9", gone());
    must_fail(
        "VA-API missing after its serial run decoded",
        matches!(after, NoRun::Unavailable(_)),
    );
    let NoRun::Failed(why) = &after else {
        panic!("{after:?}")
    };
    assert!(
        why.contains("`pair decode clip 9, VA-API download, serial` measured on this machine")
            && why.contains("it went"),
        "{why}"
    );
    // A refusal of the clip it decoded fails too, and one of another
    // clip stays a refusal: one mode's level can be past the device.
    let again = measured.judge(On::VaApi, "clip 9", refused());
    must_fail(
        "clip 9 refused after it decoded",
        matches!(again, NoRun::Refused(_)),
    );
    assert_eq!(measured.judge(On::VaApi, "clip 4", refused()), refused());
    let device_gone = measured.judge(On::VaApi, "clip 4", gone());
    assert!(matches!(device_gone, NoRun::Failed(_)), "{device_gone:?}");
    // Another path is judged by its own runs, and a failure stays one.
    assert_eq!(measured.judge(On::Vulkan, "clip 9", gone()), gone());
    let failed = NoRun::Failed("lens 1 ended".into());
    assert_eq!(measured.judge(On::VaApi, "clip 9", failed.clone()), failed);
    // The bench's runs go through it: VA-API on a node that isn't there,
    // after VA-API decoded, fails, and its rows aren't recorded.
    let plan = small_plan(Vec::new(), &media_dir());
    let spec = RunSpec {
        on: On::VaApi,
        node: Some(PathBuf::from("/nonexistent/renderD128")),
        ..spec_64(PAIR_64_MATCHED, Setup::Threaded)
    };
    let mut report = Report::default();
    let named = (
        "pair64",
        "pair decode pair64, VA-API download, a thread a lens",
    );
    let mut fresh = Measured::default();
    let first = plan.measure(named, &spec, 1, &mut fresh, &mut report);
    assert!(matches!(first, Err(NoRun::Unavailable(_))), "{first:?}");
    let mut decoded = Measured::default();
    decoded.add(
        On::VaApi,
        "pair64",
        "pair decode pair64, VA-API download, serial",
    );
    let later = plan.measure(named, &spec, 1, &mut decoded, &mut report);
    must_fail(
        "VA-API missing in measure after it decoded",
        matches!(later, Err(NoRun::Unavailable(_))),
    );
    assert!(matches!(later, Err(NoRun::Failed(_))), "{later:?}");
    assert!(report.rows.is_empty(), "{:?}", report.rows);
}

#[test]
fn a_run_in_a_child_process_measures_its_own_memory() {
    let _time = timed("a_run_in_a_child_process_measures_its_own_memory");
    let spec = spec_64(PAIR_64_MATCHED, Setup::Threaded);
    let ran = in_child(&spec).unwrap();
    let frames: Vec<u64> = ran.lenses.iter().map(|l| l.frames).collect();
    assert_eq!(frames, [30, 30]);
    assert!(ran.peak_held <= 2 * HELD_A_LENS, "{ran:?}");
    match ran.rss_kib {
        Some(kib) => assert!(kib > 0, "{ran:?}"),
        None => eprintln!("the peak can't be reset here: {CLEAR_REFS} isn't writable"),
    }
    // A file that isn't there fails the run; a device that isn't there
    // makes the path unavailable.
    let missing = RunSpec {
        file: media_dir().join("not-there.mp4"),
        ..spec.clone()
    };
    let why = must_fail("a missing file", in_child(&missing));
    assert!(matches!(why, NoRun::Failed(_)), "{why:?}");
    let no_device = RunSpec {
        on: On::VaApi,
        node: Some(PathBuf::from("/nonexistent/renderD128")),
        ..spec
    };
    let why = must_fail("VA-API on a node that isn't there", in_child(&no_device));
    assert!(matches!(why, NoRun::Unavailable(_)), "{why:?}");
}

#[test]
fn the_encode_beside_takes_frames_and_leaves_no_file() {
    let _time = timed("the_encode_beside_takes_frames_and_leaves_no_file");
    let scratch = Scratch::new("pair-decode-beside");
    let beside = Beside {
        kind: EncoderKind::Software,
        size: (64, 64),
    };
    let stop = AtomicBool::new(false);
    let (started, has_started) = mpsc::channel();
    let listing = || std::fs::read_dir(&scratch.0).unwrap().count();
    let (taken, window) = thread::scope(|s| {
        let e = s.spawn(|| encode_beside(beside, None, &scratch.0, &stop, started));
        has_started.recv().unwrap();
        let from = Instant::now();
        thread::sleep(Duration::from_millis(200));
        let window = (from, Instant::now());
        // The control for the listing below: while the encode runs, its
        // file is there.
        must_fail("the listing while the encode runs", listing() == 0);
        stop.store(true, Ordering::SeqCst);
        (e.join().unwrap(), window)
    });
    let taken = taken.unwrap();
    // Paced at 30 fps: about 6 frames in 0.2 s.
    let encoded = taken.over(window);
    let held = at_least(encoded.frames, 3);
    assert_eq!(held.passes(), Some(true), "{held}: {encoded:?}");
    let paced = at_most(encoded.frames as f64, 30.0 * encoded.secs + 2.0);
    assert_eq!(paced.passes(), Some(true), "{paced}: {encoded:?}");
    assert_eq!(listing(), 0);
    // The gate's encoder on a node that isn't there doesn't start.
    let vaapi = Beside {
        kind: EncoderKind::Vaapi,
        ..beside
    };
    let node = Path::new("/nonexistent/renderD128");
    let why = must_fail(
        "VA-API's encoder on no device",
        probe_beside(vaapi, Some(node), &scratch.0),
    );
    assert!(why.contains("no VA-API device"), "{why}");
    probe_beside(beside, None, &scratch.0).unwrap();
}

#[test]
fn the_encode_beside_is_rated_over_the_decodes_window() {
    let _time = timed("the_encode_beside_is_rated_over_the_decodes_window");
    let t = Instant::now();
    let at = |s: f64| t + Duration::from_secs_f64(s);
    // Paced at 30 fps from 0 s, while the decoders open, to 14 s; the
    // decode's window is 2 s to 12 s.
    let paced = |until: f64| {
        (1..)
            .map(|n| f64::from(n) / 30.0)
            .take_while(move |&s| s <= until)
    };
    let window = (at(2.0), at(12.0));
    let kept = Taken(paced(14.0).map(at).collect());
    let held = kept.over(window);
    assert_eq!(held.frames, 300);
    assert!((held.fps() - 30.0).abs() < 1e-9, "{held:?}");
    assert_eq!(at_least(held.fps(), ENCODE_FLOOR_FPS).passes(), Some(true));
    // The control: an encode that stalls for the window's last 3 s and
    // catches up after it took 21 frames a second there, though over its
    // whole run, first frame to last, it averages 29.6.
    let mut stalled: Vec<Instant> = paced(9.0).map(at).collect();
    stalled.extend(std::iter::repeat_n(at(12.1), 90));
    let stalled = Taken(stalled);
    let whole = (stalled.0.len() - 1) as f64
        / stalled.0[stalled.0.len() - 1]
            .duration_since(stalled.0[0])
            .as_secs_f64();
    assert_eq!(
        at_least(whole, ENCODE_FLOOR_FPS).passes(),
        Some(true),
        "{whole}"
    );
    let fell = stalled.over(window);
    assert_eq!(fell.frames, 210);
    must_fail(
        "an encode stalled for the window's last 3 s",
        at_least(fell.fps(), ENCODE_FLOOR_FPS),
    );
    // A window of no length has no rate, which decides nothing.
    assert!(kept.over((at(2.0), at(2.0))).fps().is_nan());
}

/// The bench's plan in software on small clips, with no doctor's checks.
fn small_plan(clips: Vec<Clip>, scratch: &Path) -> Plan {
    Plan {
        clips,
        paths: vec![On::VaApi, On::Software],
        node: Some(PathBuf::from("/nonexistent/renderD128")),
        beside: Ok(Beside {
            kind: EncoderKind::Software,
            size: (64, 64),
        }),
        scratch: scratch.to_path_buf(),
        frames: 30,
        gated_runs: GATED_RUNS,
        doctor: Doctor::default(),
        echo: false,
    }
}

#[test]
fn the_bench_runs_its_rows_and_verdicts_on_small_clips() {
    let _time = timed("the_bench_runs_its_rows_and_verdicts_on_small_clips");
    let scratch = Scratch::new("pair-decode-bench");
    let clip = |name: &str, gate, unlike| Clip {
        name: name.to_string(),
        file: Ok(pair_64(name)),
        gate,
        unlike,
    };
    let plan = small_plan(
        vec![
            clip(PAIR_64_MATCHED, Some(Gate::K1K4), None),
            clip(PAIR_64_GOP_FAULT, Some(Gate::K2), None),
            clip(PAIR_64_LENS_SHORT, None, Some(PERF_PAIR_P_ONLY)),
            Clip {
                name: "a clip that isn't here".into(),
                file: Err("it isn't".into()),
                gate: None,
                unlike: None,
            },
        ],
        &scratch.0,
    );
    let report = plan.run();
    for r in &report.rows {
        let values: Vec<String> = r.values.iter().map(|v| format!("{v:.1}")).collect();
        eprintln!("{}\t{}\t{}", r.name, values.join(" "), r.unit);
    }
    for n in &report.notes {
        eprintln!("# {n}");
    }
    assert_eq!(report.failed, Vec::<String>::new());
    let row = |name: &str| {
        report
            .rows
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no row `{name}`"))
    };
    let gated = format!("pair decode {PAIR_64_MATCHED}, software, a thread a lens");
    let fps = row(&format!("{gated}: slower lens beside the encode"));
    assert_eq!(fps.values.len(), GATED_RUNS);
    assert!(fps.values.iter().all(|&f| f > 0.0), "{fps:?}");
    assert_eq!(row(&format!("{gated}: opening")).values.len(), GATED_RUNS);
    // A run of 30 small frames is over before the encode's second frame
    // is due, so its rate over the decode's window says little: the
    // encode tests measure that.
    let encode = row(&format!(
        "{gated}: the encode beside, in the decode's window"
    ));
    assert_eq!(encode.values.len(), GATED_RUNS);
    let short = format!("pair decode {PAIR_64_LENS_SHORT}, software, serial: slower lens alone");
    assert_eq!(row(&short).values.len(), 1);
    let k3 = format!("pair decode {PAIR_64_MATCHED}, software, lens 0 only, for K3: lens 0 alone");
    assert_eq!(row(&k3).values.len(), 1);
    let noted = |text: &str| report.notes.iter().any(|n| n.contains(text));
    assert!(noted(&format!(
        "pair decode {PAIR_64_MATCHED}, VA-API download: not measured"
    )));
    assert!(noted(
        "pair decode a clip that isn't here: not measured: it isn't"
    ));
    assert!(noted(&format!(
        "pair decode {PAIR_64_LENS_SHORT}: 29 frames a lens"
    )));
    assert!(noted(
        "K1's rule (odin's export), on this machine, pair64-matched beside the encode: not \
         decided"
    ));
    assert!(noted(
        "K4's rule (huginn's 8K30 export), on this machine, pair64-matched beside the encode: \
         not decided"
    ));
    assert!(noted(
        "K2's rule for its export half (huginn's 5.7K30; its engine half is \
         bench_playback_x5_raw's), on this machine"
    ));
    // The clip coded unlike the X5 says so beside its rows, and only it:
    // the control.
    assert!(noted(&format!(
        "pair decode {PAIR_64_LENS_SHORT}: {PERF_PAIR_P_ONLY}"
    )));
    must_fail(
        "a clip coded as the X5 is said to be P only",
        noted(&format!(
            "pair decode {PAIR_64_MATCHED}: {PERF_PAIR_P_ONLY}"
        )),
    );
    // K3's runs had every frame alive at once, so that where the peak
    // can be measured K3 decides.
    let k3_said = report
        .notes
        .iter()
        .find(|n| n.starts_with("K3 (memory) on software"))
        .unwrap();
    if report.rows.iter().any(|r| r.unit == "MiB") {
        assert!(
            k3_said.contains(&format!(
                "each pair run had {} frames alive at once",
                2 * HELD_A_LENS
            )),
            "{k3_said}"
        );
    }
    // Without hardware K1, K2 and K4 are undecided, and named so; K3 in
    // software decides where the peak can be measured, so the bench
    // skips only where it can't.
    let undecided = report.undecided();
    for k in ["K1", "K2's export half", "K4"] {
        assert!(undecided.contains(&k), "{undecided:?}");
    }
    assert!(noted(
        "left undecided on this machine: K1, K2's export half"
    ));
    let k3_decided = !undecided.contains(&"K3 on software");
    assert_eq!(
        k3_decided,
        report.rows.iter().any(|r| r.unit == "MiB"),
        "{undecided:?}"
    );
    // The control for the rows found above: a clip that isn't here has
    // none.
    let mut rows = report
        .rows
        .iter()
        .filter(|r| r.name.starts_with("pair decode a clip"));
    must_fail("rows for a clip that isn't here", rows.next().is_some());
}
