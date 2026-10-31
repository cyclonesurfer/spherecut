//! The toolkit check's prototype (D26; M2 plan section 12 and amendment
//! 3): a timeline of 6 tracks and 500 clips, drawn as the editor will
//! draw one, to learn before the GUI work whether egui holds the
//! display's refresh with it. `spherecut --timeline-proto` opens it to
//! try by hand. With `--proto-out`, a script drives it for
//! `--proto-seconds` (a zoom sweep, a clip dragged across the width with
//! snapping, a ripple trim and a pan, over and over), what each UI pass
//! took goes to that file as one JSON document
//! (`spherecut.timeline-proto/1`), and the window closes. hw-test's
//! `gui_proto` runs it on real desktops; kittest runs it here.
//!
//! Each clip is a rounded rect with its name and a strip of its own
//! thumbnail, one of 500 distinct 160×90 textures. There is a ruler, a
//! playhead that moves at 30 fps, and an overlay of the last second's
//! UI times. A moved clip snaps to every other clip's edges and to the
//! playhead, with at most 4 candidates drawn; a trim ripples the clips
//! after it on its track; zoom goes from the whole timeline to 2 s on
//! screen.
//!
//! Each UI pass measures three times: the interval since the pass
//! before; eframe's count of the pass's own work (`cpu_usage`: building
//! the UI, tessellating and painting, without the surface's acquire and
//! present), which comes a pass late; and building, this module's own
//! code. Work less building is egui's end of pass, tessellation and
//! painting, which eframe doesn't split. The first second
//! ([`SETTLE`]) is left out, as the smoke run leaves it out.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use egui::{Color32, Event, FontId, Pos2, Rect, Stroke, pos2, vec2};
use serde::Serialize;

use crate::jobs::{Job, Poll};
use crate::smoke::{Adapter, Outcome, Percentiles, SCREENSHOT_WIDTH, SETTLE};

/// The document's schema.
pub const SCHEMA: &str = "spherecut.timeline-proto/1";

pub const TRACKS: usize = 6;
pub const CLIPS: usize = 500;

/// Each clip's thumbnail, in pixels.
pub const THUMB: [usize; 2] = [160, 90];

/// The timeline every run draws.
pub const SEED: u64 = 0x5EED_0D26;

/// The shortest a trim leaves a clip, in seconds.
pub const MIN_LEN_S: f64 = 0.5;

/// The fewest seconds on screen, zoomed in all the way.
pub const MIN_SPAN_S: f64 = 2.0;

/// Within this many points of an edge, a moved clip snaps to it.
pub const SNAP_PT: f32 = 8.0;

/// The snap candidates drawn, at most: those nearest the moved clip's
/// edges within three times [`SNAP_PT`].
pub const SNAP_DRAWN: usize = 4;

/// The playhead's steps a second.
pub const PLAYHEAD_FPS: f64 = 30.0;

/// The script repeats every this many seconds ([`TimelineProto::script`]).
pub const CYCLE_S: f64 = 12.0;

/// The tracks' names, top to bottom.
const TRACK_NAMES: [&str; TRACKS] = ["V1", "V2", "V3", "A1", "A2", "A3"];

const RULER_PT: f32 = 22.0;
const TRACK_PT: f32 = 56.0;
const GAP_PT: f32 = 4.0;
const HEADER_PT: f32 = 48.0;
/// The strip at a clip's top that its name is written in.
const NAME_PT: f32 = 16.0;
/// How near a clip's edge a press trims it, in points.
const EDGE_PT: f32 = 6.0;
/// A clip this wide or more has thumbnails, and this wide a name.
const THUMBS_FROM_PT: f32 = 8.0;
const NAME_FROM_PT: f32 = 40.0;

/// The sparkline's intervals.
const SPARK: usize = 240;

/// The longest the screenshot is waited for.
const SCREENSHOT: Duration = Duration::from_secs(5);

/// The script's zoom sweep ends at this many times the whole timeline's
/// zoom, where its drag and trim work.
const WORK_ZOOM: f64 = 12.0;

/// splitmix64: the timeline is the same on every machine.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: u32,
    pub start: f64,
    pub len: f64,
    name: String,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.len
    }
}

/// Which edge of a clip a trim holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// The tracks, each in order of its clips' starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub tracks: Vec<Vec<Clip>>,
}

impl Timeline {
    /// [`CLIPS`] clips of 1 to 30 s (as many of each length's doubling)
    /// on [`TRACKS`] tracks, three in ten after a gap of up to 4 s.
    pub fn generate(seed: u64) -> Timeline {
        let mut rng = Rng(seed);
        let mut tracks = vec![Vec::new(); TRACKS];
        let mut ends = [0.0f64; TRACKS];
        for id in 0..CLIPS as u32 {
            let t = (rng.next() % TRACKS as u64) as usize;
            let len = 30f64.powf(rng.unit());
            let gap = if rng.unit() < 0.3 {
                rng.unit() * 4.0
            } else {
                0.0
            };
            let start = ends[t] + gap;
            ends[t] = start + len;
            tracks[t].push(Clip {
                id,
                start,
                len,
                name: format!("clip {id:03}"),
            });
        }
        Timeline { tracks }
    }

    pub fn clips(&self) -> usize {
        self.tracks.iter().map(Vec::len).sum()
    }

    /// The last clip's end.
    pub fn end(&self) -> f64 {
        self.tracks
            .iter()
            .flatten()
            .map(Clip::end)
            .fold(0.0, f64::max)
    }

    /// Clip `id` on `track` starts at `start`, or 0, and the track stays
    /// in order. Clips may overlap.
    pub fn move_clip(&mut self, track: usize, id: u32, start: f64) {
        let Some(clips) = self.tracks.get_mut(track) else {
            return;
        };
        if let Some(c) = clips.iter_mut().find(|c| c.id == id) {
            c.start = start.max(0.0);
        }
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    }

    /// Every clip's start and end but `except`'s, sorted.
    pub fn edges(&self, except: u32) -> Vec<f64> {
        let mut e: Vec<f64> = self
            .tracks
            .iter()
            .flatten()
            .filter(|c| c.id != except)
            .flat_map(|c| [c.start, c.end()])
            .collect();
        e.sort_by(f64::total_cmp);
        e
    }

    /// Whether each track is in order and every clip at least
    /// [`MIN_LEN_S`] long, from 0.
    pub fn is_valid(&self) -> bool {
        self.tracks.iter().all(|t| {
            t.windows(2).all(|w| w[0].start <= w[1].start)
                && t.iter().all(|c| c.start >= 0.0 && c.len >= MIN_LEN_S)
        })
    }
}

/// A ripple trim: `track` becomes `original` with clip `idx`'s `edge`
/// moved by `delta` seconds, and every clip after it moved by as much as
/// its length changed. The clip keeps its start, and at least
/// [`MIN_LEN_S`]; trimming the start trims the clip's head.
pub fn ripple(track: &mut Vec<Clip>, original: &[Clip], idx: usize, edge: Edge, delta: f64) {
    track.clear();
    track.extend_from_slice(original);
    let Some(c) = track.get_mut(idx) else {
        return;
    };
    let len = match edge {
        Edge::End => c.len + delta,
        Edge::Start => c.len - delta,
    }
    .max(MIN_LEN_S);
    let shift = len - c.len;
    c.len = len;
    for later in &mut track[idx + 1..] {
        later.start = (later.start + shift).max(0.0);
    }
    track.sort_by(|a, b| a.start.total_cmp(&b.start));
}

/// The nearest of sorted `edges` to `t` within `within`.
pub fn snap(edges: &[f64], t: f64, within: f64) -> Option<f64> {
    let i = edges.partition_point(|&e| e < t);
    [i.checked_sub(1), Some(i)]
        .into_iter()
        .flatten()
        .filter_map(|j| edges.get(j).copied())
        .filter(|e| (e - t).abs() <= within)
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// Up to `n` of sorted `edges` nearest any of `ts`, each within `within`
/// of one, nearest first.
fn near(edges: &[f64], ts: &[f64], within: f64, n: usize) -> Vec<f64> {
    let mut found: Vec<(f64, f64)> = Vec::new();
    for &t in ts {
        let lo = edges.partition_point(|&e| e < t - within);
        let hi = edges.partition_point(|&e| e <= t + within);
        for &e in &edges[lo..hi] {
            let d = (e - t).abs();
            match found.iter_mut().find(|(f, _)| *f == e) {
                Some(f) => f.1 = f.1.min(d),
                None => found.push((e, d)),
            }
        }
    }
    found.sort_by(|a, b| a.1.total_cmp(&b.1));
    found.into_iter().take(n).map(|(e, _)| e).collect()
}

/// What part of the timeline is on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// The time at the tracks' left edge, in seconds.
    pub left_s: f64,
    pub pt_per_s: f64,
}

impl View {
    /// The whole timeline, `end` seconds long, in `width` points.
    pub fn fit(end: f64, width: f32) -> View {
        View {
            left_s: 0.0,
            pt_per_s: f64::from(width.max(1.0)) / Self::room(end),
        }
    }

    /// `end` with room after it to move or trim the last clip into.
    fn room(end: f64) -> f64 {
        end * 1.02 + 2.0
    }

    /// Seconds on screen.
    pub fn span_s(&self, width: f32) -> f64 {
        f64::from(width) / self.pt_per_s
    }

    /// Between the whole timeline and [`MIN_SPAN_S`] on screen, and no
    /// further left than 0 or right than the end.
    pub fn clamp(&mut self, end: f64, width: f32) {
        let w = f64::from(width.max(1.0));
        let room = Self::room(end);
        self.pt_per_s = self.pt_per_s.clamp(w / room, w / MIN_SPAN_S);
        self.left_s = self.left_s.clamp(0.0, (room - w / self.pt_per_s).max(0.0));
    }

    /// Zoom by `factor` about `x` points from the tracks' left, which
    /// keeps the time under it unless the clamp moves it.
    pub fn zoom_about(&mut self, factor: f64, x: f32, end: f64, width: f32) {
        let t = self.time_at(x);
        self.pt_per_s *= factor;
        self.clamp(end, width);
        self.left_s = t - f64::from(x) / self.pt_per_s;
        self.clamp(end, width);
    }

    /// Points from the tracks' left.
    pub fn x_of(&self, t: f64) -> f64 {
        (t - self.left_s) * self.pt_per_s
    }

    pub fn time_at(&self, x: f32) -> f64 {
        self.left_s + f64::from(x) / self.pt_per_s
    }
}

/// The ruler's labelled step: the shortest that leaves 80 points
/// between labels.
pub fn tick_step(pt_per_s: f64) -> f64 {
    const STEPS: [f64; 14] = [
        0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1200.0,
    ];
    STEPS
        .into_iter()
        .find(|s| s * pt_per_s >= 80.0)
        .unwrap_or(STEPS[STEPS.len() - 1])
}

/// "m:ss", or "m:ss.d" for steps under a second.
fn timecode(t: f64, step: f64) -> String {
    let tenths = (t * 10.0).round() as u64;
    let (m, s, d) = (tenths / 600, tenths / 10 % 60, tenths % 10);
    if step < 1.0 {
        format!("{m}:{s:02}.{d}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A clip's thumbnail: a sky and a ground in its own hues, a sun and
/// stripes where its id puts them.
fn thumbnail(id: u32) -> egui::ColorImage {
    let [w, h] = THUMB;
    let hue = (id as f32 * 0.618_034).fract();
    let horizon = h * (9 + (id as usize * 13) % 7) / 20;
    let sun = ((id as usize * 37) % w, horizon / 2);
    let stripe = 4 + id as usize % 9;
    let shade =
        |hue: f32, v: f32| -> Color32 { egui::ecolor::Hsva::new(hue.fract(), 0.55, v, 1.0).into() };
    let sky: Vec<Color32> = (0..h)
        .map(|y| shade(hue, 1.0 - 0.5 * y as f32 / h as f32))
        .collect();
    let ground = [shade(hue + 0.3, 0.45), shade(hue + 0.3, 0.6)];
    let sun_color = Color32::from_rgb(255, 238, 190);
    let mut pixels = Vec::with_capacity(w * h);
    for (y, sky) in sky.iter().enumerate() {
        for x in 0..w {
            let (dx, dy) = (x.abs_diff(sun.0), y.abs_diff(sun.1));
            pixels.push(if y >= horizon {
                ground[(x / stripe + y / stripe) % 2]
            } else if dx * dx + dy * dy < 81 {
                sun_color
            } else {
                *sky
            });
        }
    }
    egui::ColorImage::new([w, h], pixels)
}

/// Where the last pass drew the tracks, right of their names and below
/// the ruler, for the script.
fn track_rect(area: Rect, track: usize) -> Rect {
    let top = area.top() + GAP_PT + track as f32 * (TRACK_PT + GAP_PT);
    Rect::from_x_y_ranges(area.x_range(), top..=top + TRACK_PT)
}

/// What a press started.
#[derive(Debug)]
enum Gesture {
    Move {
        track: usize,
        id: u32,
        /// From the clip's start to the press, in seconds.
        grab_s: f64,
        len: f64,
        /// Every other clip's edges, from the press.
        edges: Vec<f64>,
        /// Drawn, and the one snapped to.
        candidates: Vec<f64>,
        snapped: Option<f64>,
    },
    Trim {
        track: usize,
        idx: usize,
        edge: Edge,
        press_s: f64,
        original: Vec<Clip>,
    },
    Scrub,
}

/// What one pass drew; the most of each in a pass, in the document.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Drawn {
    pub clips: usize,
    pub thumbnails: usize,
    pub names: usize,
    pub snap_lines: usize,
}

impl Drawn {
    fn max(self, o: Drawn) -> Drawn {
        Drawn {
            clips: self.clips.max(o.clips),
            thumbnails: self.thumbnails.max(o.thumbnails),
            names: self.names.max(o.names),
            snap_lines: self.snap_lines.max(o.snap_lines),
        }
    }
}

/// What was done to the timeline, by hand or by the script.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Actions {
    pub moves: u64,
    pub trims: u64,
    pub scrubs: u64,
    /// UI passes that zoomed or panned.
    pub zoom_passes: u64,
    pub pan_passes: u64,
    /// Times a moved clip came to snap.
    pub snaps: u64,
}

/// One UI pass's times, in milliseconds.
#[derive(Debug, Clone, Copy)]
struct Pass {
    at: f64,
    interval: Option<f32>,
    work: Option<f32>,
    build: f32,
}

/// What `--timeline-proto` and its flags ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtoArgs {
    /// How long the script runs, from the first pass.
    pub seconds: f64,
    /// Where the JSON goes; without it, no script runs and the window
    /// stays open.
    pub out: Option<PathBuf>,
    /// Where the screenshot goes, as a PNG.
    pub screenshot: Option<PathBuf>,
    /// egui's zoom factor: points this many times the desktop's.
    pub zoom: f32,
}

/// The script's state ([`TimelineProto::script`]).
#[derive(Debug, Default)]
struct Script {
    /// The time of its first pass.
    t0: Option<f64>,
    cycle: Option<u64>,
    /// The zoom when the cycle began.
    zoom_from: f64,
    /// The step holding the button down, and where it pressed.
    held: Option<(Step, Pos2)>,
    pointer: Option<Pos2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Zoom,
    Drag,
    Trim,
    Pan,
}

/// A scripted, measured run.
#[derive(Debug)]
struct Run {
    out: PathBuf,
    seconds: f64,
    screenshot: Option<PathBuf>,
    script: Script,
    phase: Phase,
    outcome: Option<Outcome>,
    /// How far into the run measuring began: the first pass past
    /// [`SETTLE`].
    from_s: Option<f64>,
    frames: u64,
    passes: u64,
    late: u64,
    interval: Vec<f32>,
    work: Vec<f32>,
    build: Vec<f32>,
    drawn: Drawn,
}

#[derive(Debug)]
enum Phase {
    Running,
    Shooting {
        doc: Box<Document>,
        asked: Instant,
        shot: mpsc::Receiver<Arc<egui::ColorImage>>,
    },
    Writing {
        job: Job<Outcome>,
    },
    Done {
        closed: bool,
    },
}

/// The thumbnails' textures by clip id; egui's handles have no Debug.
#[derive(Default)]
struct Textures(Vec<egui::TextureHandle>);

impl std::fmt::Debug for Textures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} textures", self.0.len())
    }
}

/// The prototype: the timeline, its view, a gesture, and the times.
#[derive(Debug)]
pub struct TimelineProto {
    pub timeline: Timeline,
    pub view: Option<View>,
    /// Each clip's thumbnail by id, until the first pass loads them.
    images: Vec<egui::ColorImage>,
    textures: Textures,
    gesture: Option<Gesture>,
    /// The tracks' area as the last pass drew it.
    area: Option<Rect>,
    /// The playhead was at `base_s` at time `since`.
    playhead: (f64, f64),
    pub actions: Actions,
    drawn: Drawn,
    /// The fewest and most seconds on screen.
    span_s: Option<[f64; 2]>,
    /// The last second's passes, and the last [`SPARK`] intervals.
    recent: VecDeque<Pass>,
    spark: VecDeque<f32>,
    last_pass: Option<Instant>,
    logic_ran: bool,
    adapter: Adapter,
    surface_format: String,
    refresh_hz: Option<f64>,
    run: Option<Run>,
}

impl TimelineProto {
    /// The seeded timeline and its 500 thumbnails, made now; their
    /// textures load in the first pass.
    pub fn new(adapter: Adapter, surface_format: String, args: &ProtoArgs) -> TimelineProto {
        let timeline = Timeline::generate(SEED);
        let images = (0..CLIPS as u32).map(thumbnail).collect();
        TimelineProto {
            timeline,
            view: None,
            images,
            textures: Textures::default(),
            gesture: None,
            area: None,
            playhead: (0.0, 0.0),
            actions: Actions::default(),
            drawn: Drawn::default(),
            span_s: None,
            recent: VecDeque::new(),
            spark: VecDeque::new(),
            last_pass: None,
            logic_ran: false,
            adapter,
            surface_format,
            refresh_hz: None,
            run: args.out.clone().map(|out| Run {
                out,
                seconds: args.seconds,
                screenshot: args.screenshot.clone(),
                script: Script::default(),
                phase: Phase::Running,
                outcome: None,
                from_s: None,
                frames: 0,
                passes: 0,
                late: 0,
                interval: Vec::new(),
                work: Vec::new(),
                build: Vec::new(),
                drawn: Drawn::default(),
            }),
        }
    }

    /// The window's refresh rate, as its monitor has it.
    pub fn set_refresh_hz(&mut self, hz: Option<f64>) {
        if hz.is_some() {
            self.refresh_hz = hz;
        }
    }

    pub fn textures(&self) -> usize {
        self.textures.0.len()
    }

    /// Whether the run has ended and its document is written.
    pub fn done(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| matches!(r.phase, Phase::Done { .. }))
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.run.as_ref().and_then(|r| r.outcome.as_ref())
    }

    /// The writer's thread, for the shutdown to join.
    pub fn take_thread(&mut self) -> Option<JoinHandle<()>> {
        let run = self.run.as_mut()?;
        if !matches!(run.phase, Phase::Writing { .. }) {
            return None;
        }
        match std::mem::replace(&mut run.phase, Phase::Done { closed: true }) {
            Phase::Writing { job } => job.into_thread(),
            _ => None,
        }
    }

    /// eframe's every pass, the window shown or not.
    pub fn logic(&mut self) {
        self.logic_ran = true;
        self.count_pass();
    }

    fn count_pass(&mut self) {
        if let Some(run) = &mut self.run
            && run.from_s.is_some()
            && matches!(run.phase, Phase::Running)
        {
            run.passes += 1;
        }
    }

    /// The script's input for the pass at `time` (egui's), from where
    /// the last pass drew things. Each [`CYCLE_S`]:
    ///
    /// - 0 to 4 s: zoom about the middle, from where it was to the whole
    ///   timeline, to [`MIN_SPAN_S`] on screen, and to [`WORK_ZOOM`]
    ///   times the whole;
    /// - 4 to 7.2 s: drag the clip on the second track nearest a quarter
    ///   of the way across to the right edge, then to the left;
    /// - 7.2 to 9.2 s: hold the end of the clip on the fourth track
    ///   ending nearest 40% of the way across, ripple it 200 points
    ///   longer, then 300 shorter;
    /// - 9.2 to 12 s: pan with the wheel, 40 points a pass, right then
    ///   left.
    ///
    /// None without a run, or once it has run its seconds.
    pub fn script(&mut self, time: f64) -> Vec<Event> {
        let (Some(run), Some(area), Some(view)) = (&mut self.run, self.area, self.view) else {
            return Vec::new();
        };
        let sc = &mut run.script;
        let t0 = *sc.t0.get_or_insert(time);
        let s = time - t0;
        let mut ev = Vec::new();
        let release = |ev: &mut Vec<Event>, at: Pos2| {
            ev.push(Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            });
        };
        if s >= run.seconds || !matches!(run.phase, Phase::Running) {
            if sc.held.take().is_some() {
                release(&mut ev, sc.pointer.unwrap_or(area.center()));
            }
            return ev;
        }
        let cycle = (s / CYCLE_S).floor() as u64;
        let c = s - cycle as f64 * CYCLE_S;
        if sc.cycle != Some(cycle) {
            sc.cycle = Some(cycle);
            sc.zoom_from = view.pt_per_s;
        }
        let step = match c {
            c if c < 4.0 => Step::Zoom,
            c if c < 7.2 => Step::Drag,
            c if c < 9.2 => Step::Trim,
            _ => Step::Pan,
        };
        if let Some((held, _)) = sc.held
            && held != step
        {
            sc.held = None;
            release(&mut ev, sc.pointer.unwrap_or(area.center()));
        }
        let width = area.width();
        let end = self.timeline.end();
        let to = |ev: &mut Vec<Event>, p: Pos2, sc: &mut Script| {
            sc.pointer = Some(p);
            ev.push(Event::PointerMoved(p));
        };
        let press = |ev: &mut Vec<Event>, at: Pos2| {
            ev.push(Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            });
        };
        match step {
            Step::Zoom => {
                to(&mut ev, area.center(), sc);
                let fit = View::fit(end, width).pt_per_s;
                let knots = [
                    (0.0, sc.zoom_from),
                    (1.0, fit),
                    (2.5, f64::from(width) / MIN_SPAN_S),
                    (4.0, fit * WORK_ZOOM),
                ];
                let target = knots
                    .windows(2)
                    .find(|k| c <= k[1].0)
                    .map_or(knots[3].1, |k| {
                        let f = (c - k[0].0) / (k[1].0 - k[0].0);
                        (k[0].1.ln() + f * (k[1].1.ln() - k[0].1.ln())).exp()
                    });
                ev.push(Event::Zoom((target / view.pt_per_s) as f32));
            }
            Step::Drag => match sc.held {
                None => {
                    let goal = area.left() + 0.25 * width;
                    if let Some(r) = clip_rects(&self.timeline, area, view, 1)
                        .into_iter()
                        .filter(|r| r.width() >= 3.0 * EDGE_PT && r.center().x > area.left())
                        .min_by(|a, b| {
                            (a.center().x - goal)
                                .abs()
                                .total_cmp(&(b.center().x - goal).abs())
                        })
                    {
                        let at = r.center();
                        to(&mut ev, at, sc);
                        press(&mut ev, at);
                        sc.held = Some((Step::Drag, at));
                    }
                }
                Some((_, at)) => {
                    let (right, left) = (area.right() - 8.0, area.left() + 8.0);
                    let x = if c < 5.6 {
                        at.x + (right - at.x) * ((c - 4.0) / 1.6) as f32
                    } else {
                        right + (left - right) * ((c - 5.6) / 1.6) as f32
                    };
                    to(&mut ev, pos2(x, at.y), sc);
                }
            },
            Step::Trim => match sc.held {
                None => {
                    let goal = area.left() + 0.4 * width;
                    let (lo, hi) = (area.left() + 0.15 * width, area.right() - 0.4 * width);
                    if let Some(r) = clip_rects(&self.timeline, area, view, 3)
                        .into_iter()
                        .filter(|r| {
                            r.width() >= 2.0 * EDGE_PT + 4.0 && (lo..=hi).contains(&r.right())
                        })
                        .min_by(|a, b| {
                            (a.right() - goal)
                                .abs()
                                .total_cmp(&(b.right() - goal).abs())
                        })
                    {
                        let at = pos2(r.right() - 2.0, r.center().y);
                        to(&mut ev, at, sc);
                        press(&mut ev, at);
                        sc.held = Some((Step::Trim, at));
                    }
                }
                Some((_, at)) => {
                    let dx = if c < 8.2 {
                        200.0 * (c - 7.2)
                    } else {
                        200.0 - 300.0 * (c - 8.2)
                    };
                    to(&mut ev, pos2(at.x + dx as f32, at.y), sc);
                }
            },
            Step::Pan => {
                to(&mut ev, area.center(), sc);
                ev.push(Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(if c < 10.6 { -40.0 } else { 40.0 }, 0.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                    modifiers: egui::Modifiers::default(),
                });
            }
        }
        ev
    }

    /// Track `track`'s clips as the view puts them, whole, in screen
    /// points.
    /// Each UI pass: the overlay, the timeline, the times; and the run's
    /// end, screenshot, document and close. `work` is eframe's count of
    /// the pass before's own work, in seconds.
    pub fn ui(&mut self, ui: &mut egui::Ui, work: Option<f32>) {
        let began = Instant::now();
        let time = ui.input(|i| i.time);
        let interval = self
            .last_pass
            .replace(began)
            .map(|l| began.saturating_duration_since(l).as_secs_f32() * 1000.0);
        if !std::mem::take(&mut self.logic_ran) {
            self.count_pass();
        }
        if self.textures.0.is_empty() {
            let ctx = ui.ctx().clone();
            self.textures.0 = std::mem::take(&mut self.images)
                .into_iter()
                .enumerate()
                .map(|(id, img)| {
                    ctx.load_texture(format!("thumb-{id}"), img, egui::TextureOptions::LINEAR)
                })
                .collect();
        }
        egui::Panel::bottom("proto-overlay").show(ui, |ui| self.overlay(ui));
        egui::CentralPanel::default().show(ui, |ui| self.timeline_ui(ui, time));
        let pass = Pass {
            at: time,
            interval,
            work: work.map(|s| s * 1000.0),
            build: began.elapsed().as_secs_f32() * 1000.0,
        };
        self.record(pass);
        self.step_run(ui.ctx(), time);
        if !self.done() {
            ui.ctx().request_repaint();
        }
    }

    fn record(&mut self, pass: Pass) {
        while self.recent.front().is_some_and(|p| p.at < pass.at - 1.0) {
            self.recent.pop_front();
        }
        self.recent.push_back(pass);
        if let Some(i) = pass.interval {
            if self.spark.len() == SPARK {
                self.spark.pop_front();
            }
            self.spark.push_back(i);
        }
        let hz = self.refresh_hz;
        let drawn = self.drawn;
        let Some(run) = &mut self.run else { return };
        if !matches!(run.phase, Phase::Running) {
            return;
        }
        let s = run.script.t0.map_or(0.0, |t0| pass.at - t0);
        if run.from_s.is_none() {
            if s >= SETTLE.as_secs_f64() {
                run.from_s = Some(s);
            }
            return;
        }
        run.frames += 1;
        if let Some(i) = pass.interval {
            run.interval.push(i);
            if hz.is_some_and(|hz| f64::from(i) > 1500.0 / hz) {
                run.late += 1;
            }
        }
        run.work.extend(pass.work);
        run.build.push(pass.build);
        run.drawn = run.drawn.max(drawn);
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui, time: f64) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let ruler = Rect::from_min_max(
            pos2(rect.left() + HEADER_PT, rect.top()),
            pos2(rect.right(), rect.top() + RULER_PT),
        );
        let area = Rect::from_min_max(pos2(ruler.left(), ruler.bottom()), rect.max);
        self.area = Some(area);
        let width = area.width();
        let end = self.timeline.end();
        let mut view = self.view.unwrap_or_else(|| View::fit(end, width));
        let (pressed, down, pos, origin, zoom, scroll) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.pointer.press_origin(),
                i.zoom_delta(),
                i.smooth_scroll_delta(),
            )
        });
        if let Some(p) = pos.filter(|p| rect.contains(*p)) {
            if zoom != 1.0 {
                view.zoom_about(f64::from(zoom), p.x - area.left(), end, width);
                self.actions.zoom_passes += 1;
            }
            let pan = scroll.x + scroll.y;
            if pan != 0.0 {
                view.left_s -= f64::from(pan) / view.pt_per_s;
                self.actions.pan_passes += 1;
            }
        }
        view.clamp(end, width);
        let span = view.span_s(width);
        let [lo, hi] = self.span_s.get_or_insert([span, span]);
        *lo = lo.min(span);
        *hi = hi.max(span);
        self.view = Some(view);

        if pressed && let Some(o) = origin.filter(|o| rect.contains(*o)) {
            // A release and a press in one pass end the gesture before.
            if let Some(g) = self.gesture.take() {
                self.ended(&g);
            }
            self.gesture = self.press(o, ruler, area, view);
        }
        if let Some(mut g) = self.gesture.take() {
            if down {
                if let Some(p) = pos {
                    let playhead = self.playhead_s(time, end);
                    self.drag(
                        &mut g,
                        view.time_at(p.x - area.left()),
                        playhead,
                        view,
                        time,
                    );
                }
                self.gesture = Some(g);
            } else {
                self.ended(&g);
            }
        }
        let playhead = self.playhead_s(time, self.timeline.end());
        self.paint(ui, rect, ruler, area, view, playhead);
    }

    fn ended(&mut self, g: &Gesture) {
        match g {
            Gesture::Move { .. } => self.actions.moves += 1,
            Gesture::Trim { .. } => self.actions.trims += 1,
            Gesture::Scrub => self.actions.scrubs += 1,
        }
    }

    /// Where the playhead is at `time`: it steps at [`PLAYHEAD_FPS`] from
    /// where it was put, and wraps at the end.
    fn playhead_s(&self, time: f64, end: f64) -> f64 {
        let (base, since) = self.playhead;
        let t = base + ((time - since) * PLAYHEAD_FPS).floor() / PLAYHEAD_FPS;
        if end > 0.0 { t.rem_euclid(end) } else { 0.0 }
    }

    /// What a press at `at` starts: a scrub in the ruler, a trim within
    /// [`EDGE_PT`] of a clip's edge (its end first), a move inside one.
    fn press(&self, at: Pos2, ruler: Rect, area: Rect, view: View) -> Option<Gesture> {
        if ruler.contains(at) {
            return Some(Gesture::Scrub);
        }
        let track = (0..TRACKS).find(|&i| track_rect(area, i).y_range().contains(at.y))?;
        let x = at.x - area.left();
        let t = view.time_at(x);
        let clips = &self.timeline.tracks[track];
        // The last drawn is on top.
        let idx = clips.iter().rposition(|c| c.start <= t && t <= c.end())?;
        let c = &clips[idx];
        let near = |s: f64| (view.x_of(s) as f32 - x).abs() <= EDGE_PT;
        let edge = if near(c.end()) {
            Some(Edge::End)
        } else if near(c.start) {
            Some(Edge::Start)
        } else {
            None
        };
        Some(match edge {
            Some(edge) => Gesture::Trim {
                track,
                idx,
                edge,
                press_s: t,
                original: clips.clone(),
            },
            None => Gesture::Move {
                track,
                id: c.id,
                grab_s: t - c.start,
                len: c.len,
                edges: self.timeline.edges(c.id),
                candidates: Vec::new(),
                snapped: None,
            },
        })
    }

    /// A held gesture, the pointer at `t` seconds: a moved clip snaps
    /// its start or end, whichever is nearer, to an edge or the playhead
    /// within [`SNAP_PT`].
    fn drag(&mut self, g: &mut Gesture, t: f64, playhead: f64, view: View, time: f64) {
        match g {
            Gesture::Move {
                track,
                id,
                grab_s,
                len,
                edges,
                candidates,
                snapped,
            } => {
                let raw = t - *grab_s;
                let within = f64::from(SNAP_PT) / view.pt_per_s;
                let to = |x: f64| {
                    let head = ((playhead - x).abs() <= within).then_some(playhead);
                    match (snap(edges, x, within), head) {
                        (Some(e), Some(h)) if (h - x).abs() < (e - x).abs() => Some(h),
                        (e, h) => e.or(h),
                    }
                };
                let by_start = to(raw).map(|e| (e, e - raw));
                let by_end = to(raw + *len).map(|e| (e, e - (raw + *len)));
                let pick = match (by_start, by_end) {
                    (Some(a), Some(b)) => Some(if a.1.abs() <= b.1.abs() { a } else { b }),
                    (a, b) => a.or(b),
                };
                let start = (raw + pick.map_or(0.0, |p| p.1)).max(0.0);
                if pick.is_some() && snapped.is_none() {
                    self.actions.snaps += 1;
                }
                *snapped = pick.map(|p| p.0);
                let ends = [start, start + *len];
                let mut drawn = near(edges, &ends, 3.0 * within, SNAP_DRAWN);
                if ends.iter().any(|e| (playhead - e).abs() <= 3.0 * within) {
                    drawn.truncate(SNAP_DRAWN - 1);
                    drawn.push(playhead);
                }
                *candidates = drawn;
                self.timeline.move_clip(*track, *id, start);
            }
            Gesture::Trim {
                track,
                idx,
                edge,
                press_s,
                original,
            } => {
                if let Some(clips) = self.timeline.tracks.get_mut(*track) {
                    ripple(clips, original, *idx, *edge, t - *press_s);
                }
            }
            Gesture::Scrub => {
                self.playhead = (t.clamp(0.0, self.timeline.end()), time);
            }
        }
    }

    fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        ruler: Rect,
        area: Rect,
        view: View,
        playhead: f64,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(22));
        paint_ruler(&painter, ruler, view);
        let mut drawn = Drawn::default();
        let moving = match &self.gesture {
            Some(Gesture::Move { id, .. }) => Some(*id),
            _ => None,
        };
        let mut lifted = None;
        for (i, clips) in self.timeline.tracks.iter().enumerate() {
            let tr = track_rect(area, i);
            let header = Rect::from_x_y_ranges(rect.left()..=area.left() - 2.0, tr.y_range());
            painter.rect_filled(header, 2.0, Color32::from_gray(40));
            painter.text(
                header.center(),
                egui::Align2::CENTER_CENTER,
                TRACK_NAMES[i],
                FontId::proportional(13.0),
                Color32::from_gray(200),
            );
            painter.rect_filled(tr, 0.0, Color32::from_gray(30));
            let on_track = painter.with_clip_rect(tr.intersect(area));
            for c in clips {
                if Some(c.id) == moving {
                    lifted = Some((i, c));
                } else {
                    paint_clip(
                        &on_track,
                        area,
                        view,
                        c,
                        i,
                        false,
                        &self.textures.0,
                        &mut drawn,
                    );
                }
            }
        }
        if let Some((i, c)) = lifted {
            let on_track = painter.with_clip_rect(track_rect(area, i).intersect(area));
            paint_clip(
                &on_track,
                area,
                view,
                c,
                i,
                true,
                &self.textures.0,
                &mut drawn,
            );
        }
        let x_of = |t: f64| area.left() + view.x_of(t) as f32;
        let shown = |x: f32| (area.left()..=area.right()).contains(&x);
        if let Some(Gesture::Move {
            candidates,
            snapped,
            ..
        }) = &self.gesture
        {
            for &e in candidates {
                let x = x_of(e);
                if shown(x) {
                    let color = if Some(e) == *snapped {
                        Color32::from_rgb(250, 210, 60)
                    } else {
                        Color32::from_white_alpha(70)
                    };
                    painter.vline(x, area.y_range(), Stroke::new(1.0, color));
                    drawn.snap_lines += 1;
                }
            }
        }
        let x = x_of(playhead);
        if shown(x) {
            let red = Color32::from_rgb(240, 80, 70);
            painter.vline(x, ruler.top()..=area.bottom(), Stroke::new(1.5, red));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(x - 5.0, ruler.top()),
                    pos2(x + 5.0, ruler.top()),
                    pos2(x, ruler.top() + 8.0),
                ],
                red,
                Stroke::NONE,
            ));
        }
        self.drawn = drawn;
    }

    /// The last second's UI times, what the last pass drew, and the
    /// intervals as a line.
    fn overlay(&self, ui: &mut egui::Ui) {
        let of = |f: fn(&Pass) -> Option<f32>| {
            let v: Vec<f32> = self.recent.iter().filter_map(f).collect();
            Percentiles::of(&v, 0)
        };
        let (interval, work, build) = (of(|p| p.interval), of(|p| p.work), of(|p| Some(p.build)));
        let ms = |v: Option<f32>| v.map_or("–".to_owned(), |v| format!("{v:.1}"));
        let span = self
            .view
            .zip(self.area)
            .map_or(0.0, |(v, a)| v.span_s(a.width()));
        let hz = self
            .refresh_hz
            .map_or(String::new(), |hz| format!(" · {hz:.0} Hz"));
        let d = self.drawn;
        let line = |ui: &mut egui::Ui, text: String| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .monospace()
                        .size(11.0)
                        .color(Color32::LIGHT_GRAY),
                )
                .extend(),
            );
        };
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                line(
                    ui,
                    format!(
                        "UI interval {} / {} / {} ms (p50 / p99 / max, the last second) · work \
                         p99 {} · building p99 {}{hz}",
                        ms(interval.p50),
                        ms(interval.p99),
                        ms(interval.max),
                        ms(work.p99),
                        ms(build.p99),
                    ),
                );
                line(
                    ui,
                    format!(
                        "{} clips, {} thumbnails, {} names drawn · {span:.1} s on screen · {} \
                         clips on {} tracks, {} textures",
                        d.clips,
                        d.thumbnails,
                        d.names,
                        self.timeline.clips(),
                        self.timeline.tracks.len(),
                        self.textures.0.len(),
                    ),
                );
            });
            ui.add_space(12.0);
            let spark: Vec<f32> = self.spark.iter().copied().collect();
            crate::overlay::sparkline(ui, "UI frame", &spark);
        });
    }

    /// After each UI pass of a run: past its seconds, the document, the
    /// screenshot, the write, and the window's close.
    fn step_run(&mut self, ctx: &egui::Context, time: f64) {
        let now = Instant::now();
        let ending = self.run.as_ref().is_some_and(|r| {
            matches!(r.phase, Phase::Running)
                && r.script.t0.is_some_and(|t0| time - t0 >= r.seconds)
        });
        if ending {
            let played = self
                .run
                .as_ref()
                .and_then(|r| r.script.t0)
                .map_or(0.0, |t0| time - t0);
            let Some(report) = self.report(ctx, played) else {
                return;
            };
            let doc = Box::new(Document::ok(report));
            let Some(run) = &mut self.run else { return };
            if run.screenshot.is_some() {
                let (tx, rx) = mpsc::channel();
                ctx.request_screenshot(move |image| {
                    let _ = tx.send(image);
                });
                run.phase = Phase::Shooting {
                    doc,
                    asked: now,
                    shot: rx,
                };
            } else {
                run.write(*doc, None);
            }
        }
        let Some(run) = &mut self.run else { return };
        match &mut run.phase {
            Phase::Running | Phase::Done { closed: true } => {}
            Phase::Shooting { asked, shot, .. } => match shot.try_recv() {
                Ok(image) => run.shot(Some(image), None),
                Err(mpsc::TryRecvError::Empty)
                    if now.saturating_duration_since(*asked) < SCREENSHOT => {}
                Err(_) => run.shot(
                    None,
                    Some(format!(
                        "no screenshot came within {} s",
                        SCREENSHOT.as_secs()
                    )),
                ),
            },
            Phase::Writing { job } => match job.poll() {
                Poll::Pending => {}
                Poll::Done(outcome) => {
                    if let Err((code, message)) = &outcome {
                        tracing::error!(code, message, "the timeline prototype's run failed");
                    }
                    run.outcome = Some(outcome);
                    run.phase = Phase::Done { closed: false };
                }
                Poll::Lost => {
                    run.outcome = Some(Err((
                        "internal",
                        "the prototype's writer ended without a result".into(),
                    )));
                    run.phase = Phase::Done { closed: false };
                }
            },
            Phase::Done { closed } => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                *closed = true;
            }
        }
    }

    /// What the run measured, `played_s` after its first pass.
    fn report(&self, ctx: &egui::Context, played_s: f64) -> Option<Report> {
        let run = self.run.as_ref()?;
        let ppp = ctx.pixels_per_point();
        let px = |v: f32| (v * ppp).round() as u32;
        let screen = ctx.content_rect().size();
        let from_s = run.from_s.unwrap_or(played_s);
        let expected = self
            .refresh_hz
            .map_or(run.passes as f64, |hz| hz * (played_s - from_s));
        Some(Report {
            version: spherecut_core::brand::VERSION,
            seconds: run.seconds,
            played_s,
            adapter: self.adapter.clone(),
            surface_format: self.surface_format.clone(),
            scale: ppp,
            native_scale: ctx.input(|i| i.viewport().native_pixels_per_point),
            zoom: ctx.zoom_factor(),
            window: [px(screen.x), px(screen.y)],
            refresh_hz: self.refresh_hz,
            tracks: self.timeline.tracks.len(),
            clips: self.timeline.clips(),
            textures: self.textures.0.len(),
            ui: UiReport {
                from_s,
                frames: run.frames,
                passes: run.passes,
                expected,
                presented: if expected > 0.0 {
                    run.frames as f64 / expected
                } else {
                    0.0
                },
                late: run.late,
                interval_ms: Percentiles::of(&run.interval, 0),
                work_ms: Percentiles::of(&run.work, 0),
                build_ms: Percentiles::of(&run.build, 0),
            },
            drawn: run.drawn,
            actions: self.actions,
            span_s: self.span_s.unwrap_or_default(),
            blocked_on_ui_thread: spherecut_core::blocking::blocked_on_ui_thread(),
            screenshot: None,
            screenshot_size: None,
        })
    }
}

impl Run {
    /// The screenshot came, or didn't: write the document.
    fn shot(&mut self, image: Option<Arc<egui::ColorImage>>, warning: Option<String>) {
        let Phase::Shooting { mut doc, .. } =
            std::mem::replace(&mut self.phase, Phase::Done { closed: false })
        else {
            return;
        };
        doc.warnings.extend(warning);
        self.write(*doc, image);
    }

    /// Write `doc`, and the screenshot with it, on a worker.
    fn write(&mut self, doc: Document, shot: Option<Arc<egui::ColorImage>>) {
        let out = self.out.clone();
        let png = self.screenshot.clone();
        self.phase = Phase::Writing {
            job: Job::spawn("proto-out", move || {
                write_out(&out, png.as_deref(), doc, shot)
            }),
        };
    }
}

/// Track `track`'s clips as the view puts them, whole, in screen points;
/// those that show in the area.
fn clip_rects(timeline: &Timeline, area: Rect, view: View, track: usize) -> Vec<Rect> {
    let tr = track_rect(area, track);
    timeline
        .tracks
        .get(track)
        .into_iter()
        .flatten()
        .map(|c| {
            let x0 = f64::from(area.left()) + view.x_of(c.start);
            Rect::from_x_y_ranges(
                x0 as f32..=(x0 + c.len * view.pt_per_s) as f32,
                tr.y_range(),
            )
        })
        .filter(|r| r.right() > area.left() && r.left() < area.right())
        .collect()
}

/// Clip `c` on `track`, through `p` (clipped to the track): its rect,
/// thumbnails from [`THUMBS_FROM_PT`] wide, its name from
/// [`NAME_FROM_PT`]; brighter, with a yellow edge, while it moves.
#[expect(
    clippy::too_many_arguments,
    reason = "one clip's drawing, called twice"
)]
fn paint_clip(
    p: &egui::Painter,
    area: Rect,
    view: View,
    c: &Clip,
    track: usize,
    lifted: bool,
    textures: &[egui::TextureHandle],
    drawn: &mut Drawn,
) {
    let tr = track_rect(area, track);
    let x0 = f64::from(area.left()) + view.x_of(c.start);
    let x1 = x0 + c.len * view.pt_per_s;
    if x1 < f64::from(area.left()) || x0 > f64::from(area.right()) {
        return;
    }
    // Clamped near the area, so that a long clip's rect stays small.
    let left = x0.max(f64::from(area.left()) - 4.0) as f32;
    let right = x1.min(f64::from(area.right()) + 4.0) as f32;
    let r = Rect::from_x_y_ranges(left..=right, tr.top() + 1.0..=tr.bottom() - 1.0);
    let w = (x1 - x0) as f32;
    let hue = track as f32 / TRACKS as f32;
    let fill: Color32 =
        egui::ecolor::Hsva::new(hue, 0.45, if lifted { 0.7 } else { 0.45 }, 1.0).into();
    p.rect_filled(r, 4.0, fill);
    drawn.clips += 1;
    let inside = p.with_clip_rect(r.shrink(1.0).intersect(p.clip_rect()));
    if w >= THUMBS_FROM_PT
        && let Some(tex) = textures.get(c.id as usize)
    {
        let th = TRACK_PT - NAME_PT - 4.0;
        let tw = f64::from(th * 16.0 / 9.0);
        let clip = inside.clip_rect();
        let k0 = ((f64::from(clip.left()) - x0) / tw).floor().max(0.0) as i64;
        let k1 = ((f64::from(clip.right()) - x0) / tw).ceil() as i64;
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        for k in k0..k1 {
            let tile = Rect::from_min_size(
                pos2((x0 + k as f64 * tw) as f32, tr.top() + NAME_PT),
                vec2(tw as f32 - 1.0, th),
            );
            inside.image(tex.id(), tile, uv, Color32::WHITE);
            drawn.thumbnails += 1;
        }
    }
    if w >= NAME_FROM_PT {
        inside.text(
            pos2(x0.max(f64::from(area.left())) as f32 + 4.0, tr.top() + 2.0),
            egui::Align2::LEFT_TOP,
            &c.name,
            FontId::proportional(12.0),
            Color32::WHITE,
        );
        drawn.names += 1;
    }
    let edge = if lifted {
        Color32::from_rgb(250, 210, 60)
    } else {
        Color32::from_gray(12)
    };
    p.rect_stroke(r, 4.0, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
}

/// Ticks at a fifth of [`tick_step`], labelled at each step.
fn paint_ruler(p: &egui::Painter, ruler: Rect, view: View) {
    p.rect_filled(ruler, 0.0, Color32::from_gray(34));
    let step = tick_step(view.pt_per_s);
    let minor = step / 5.0;
    let last = view.time_at(ruler.width());
    let mut k = (view.left_s / minor).floor() as i64;
    loop {
        let t = k as f64 * minor;
        if t > last {
            break;
        }
        let x = ruler.left() + view.x_of(t) as f32;
        let major = k.rem_euclid(5) == 0;
        let tall = if major { 10.0 } else { 4.0 };
        p.vline(
            x,
            ruler.bottom() - tall..=ruler.bottom(),
            Stroke::new(1.0, Color32::from_gray(150)),
        );
        if major {
            p.text(
                pos2(x + 3.0, ruler.top() + 2.0),
                egui::Align2::LEFT_TOP,
                timecode(t, step),
                FontId::monospace(11.0),
                Color32::from_gray(200),
            );
        }
        k += 1;
    }
}

/// The document: the CLI's envelope (`docs/cli-json.md`). The run has no
/// failure of its own; a write that fails is the process's.
#[derive(Debug, Serialize)]
pub struct Document {
    schema: &'static str,
    ok: bool,
    result: Report,
    warnings: Vec<String>,
}

impl Document {
    fn ok(result: Report) -> Document {
        Document {
            schema: SCHEMA,
            ok: true,
            result,
            warnings: Vec::new(),
        }
    }
}

/// What the run measured.
#[derive(Debug, Serialize)]
pub struct Report {
    pub version: &'static str,
    /// Asked for, and run.
    pub seconds: f64,
    pub played_s: f64,
    pub adapter: Adapter,
    pub surface_format: String,
    /// Pixels a point; the window's own scale factor; egui's zoom on it.
    pub scale: f32,
    pub native_scale: Option<f32>,
    pub zoom: f32,
    /// The window's inner size, in physical pixels.
    pub window: [u32; 2],
    pub refresh_hz: Option<f64>,
    pub tracks: usize,
    pub clips: usize,
    pub textures: usize,
    pub ui: UiReport,
    /// The most drawn in one pass.
    pub drawn: Drawn,
    pub actions: Actions,
    /// The fewest and most seconds on screen.
    pub span_s: [f64; 2],
    /// Blocking calls on the UI thread since the start: 0 unless
    /// something is wrong (`core::blocking`).
    pub blocked_on_ui_thread: u64,
    pub screenshot: Option<String>,
    pub screenshot_size: Option<[u32; 2]>,
}

/// The UI passes after the first second.
#[derive(Debug, Serialize)]
pub struct UiReport {
    /// How far into the run measuring began.
    pub from_s: f64,
    /// UI passes, and all passes: eframe runs only the logic while the
    /// compositor shows nothing of the window.
    pub frames: u64,
    pub passes: u64,
    /// The UI frames a display at `refresh_hz` shows in the time, or
    /// without a rate, the passes; and the share of them that ran.
    pub expected: f64,
    pub presented: f64,
    /// Intervals over one and a half refresh periods.
    pub late: u64,
    /// Between one UI pass and the next; each pass's own work, as eframe
    /// counts it; and building the UI, this module's code.
    pub interval_ms: Percentiles,
    pub work_ms: Percentiles,
    pub build_ms: Percentiles,
}

/// The writer: the screenshot, then the document that names it.
fn write_out(
    out: &std::path::Path,
    screenshot: Option<&std::path::Path>,
    mut doc: Document,
    shot: Option<Arc<egui::ColorImage>>,
) -> Outcome {
    if let (Some(path), Some(image)) = (screenshot, shot) {
        let small = crate::smoke::downscale(&image, SCREENSHOT_WIDTH);
        match crate::smoke::write_png(path, &small) {
            Ok(()) => {
                doc.result.screenshot = Some(path.display().to_string());
                doc.result.screenshot_size = Some([small.width(), small.height()]);
            }
            Err(e) => doc
                .warnings
                .push(format!("the screenshot can't be written: {e}")),
        }
    }
    let mut text = serde_json::to_string_pretty(&doc)
        .map_err(|e| ("internal", format!("the prototype's JSON: {e}")))?;
    text.push('\n');
    crate::smoke::write_file(out, text.as_bytes())
        .map_err(|e| ("io_error", format!("writing {}: {e}", out.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(id: u32, start: f64, len: f64) -> Clip {
        Clip {
            id,
            start,
            len,
            name: format!("clip {id:03}"),
        }
    }

    #[test]
    fn the_timeline_is_the_seeds_and_every_track_is_in_order() {
        let t = Timeline::generate(SEED);
        assert_eq!(t, Timeline::generate(SEED));
        assert_ne!(t, Timeline::generate(SEED + 1));
        assert_eq!((t.tracks.len(), t.clips()), (TRACKS, CLIPS));
        assert!(t.is_valid());
        let mut ids: Vec<u32> = t.tracks.iter().flatten().map(|c| c.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..CLIPS as u32).collect::<Vec<_>>());
        for track in &t.tracks {
            assert!(track.len() > 40, "{}", track.len());
            for w in track.windows(2) {
                // Laid end to end, some after a gap.
                assert!(w[1].start >= w[0].end() - 1e-9);
            }
            assert!(track.iter().all(|c| (1.0..=30.0).contains(&c.len)));
        }
        // Long enough that 2 s on screen is a deep zoom.
        assert!((400.0..2000.0).contains(&t.end()), "{}", t.end());
    }

    #[test]
    fn a_ripple_trim_moves_the_clips_after_it_by_the_change() {
        let original = vec![clip(0, 0.0, 4.0), clip(1, 4.0, 2.0), clip(2, 7.0, 3.0)];
        let mut track = original.clone();
        ripple(&mut track, &original, 0, Edge::End, 1.5);
        assert_eq!(track[0].len, 5.5);
        assert_eq!((track[1].start, track[2].start), (5.5, 8.5));
        // From the original each time: a drag back undoes it.
        ripple(&mut track, &original, 0, Edge::End, 0.0);
        assert_eq!(track, original);
        // The head: the clip keeps its start, and shortens.
        ripple(&mut track, &original, 1, Edge::Start, 0.5);
        assert_eq!((track[1].start, track[1].len), (4.0, 1.5));
        assert_eq!(track[2].start, 6.5);
        assert_eq!(track[0], original[0]);
        // Never below the shortest, and the rest move only as far.
        ripple(&mut track, &original, 1, Edge::End, -10.0);
        assert_eq!(track[1].len, MIN_LEN_S);
        assert_eq!(track[2].start, 7.0 - (2.0 - MIN_LEN_S));
    }

    #[test]
    fn a_moved_clip_keeps_its_track_in_order() {
        let mut t = Timeline {
            tracks: vec![vec![
                clip(0, 0.0, 2.0),
                clip(1, 2.0, 2.0),
                clip(2, 4.0, 2.0),
            ]],
        };
        t.move_clip(0, 0, 5.0);
        assert_eq!(
            t.tracks[0].iter().map(|c| c.id).collect::<Vec<_>>(),
            [1, 2, 0]
        );
        t.move_clip(0, 2, -3.0);
        assert_eq!(t.tracks[0][0].id, 2);
        assert_eq!(t.tracks[0][0].start, 0.0);
        assert!(t.is_valid());
        assert_eq!(t.edges(1), [0.0, 2.0, 5.0, 7.0]);
    }

    #[test]
    fn a_snap_takes_the_nearest_edge_within_reach() {
        let edges = [1.0, 2.0, 2.5, 10.0];
        assert_eq!(snap(&edges, 2.2, 0.3), Some(2.0));
        assert_eq!(snap(&edges, 2.3, 0.3), Some(2.5));
        assert_eq!(snap(&edges, 5.0, 0.3), None);
        assert_eq!(snap(&edges, 0.8, 0.2), Some(1.0));
        assert_eq!(snap(&edges, 10.5, 0.4), None);
        assert_eq!(snap(&[], 1.0, 1.0), None);
        // Nearest first, from either end, each once, at most n.
        assert_eq!(near(&edges, &[2.2, 9.9], 1.25, 4), [10.0, 2.0, 2.5, 1.0]);
        assert_eq!(near(&edges, &[2.2, 9.9], 1.0, 4), [10.0, 2.0, 2.5]);
        assert_eq!(near(&edges, &[2.2], 1.0, 2), [2.0, 2.5]);
    }

    #[test]
    fn zoom_keeps_the_time_under_the_pointer_between_the_whole_and_two_seconds() {
        let (end, width) = (600.0, 1000.0);
        let mut v = View::fit(end, width);
        assert!((v.span_s(width) - View::room(end)).abs() < 1e-9);
        let t = v.time_at(250.0);
        v.zoom_about(4.0, 250.0, end, width);
        assert!((v.time_at(250.0) - t).abs() < 1e-9);
        assert!((v.span_s(width) - View::room(end) / 4.0).abs() < 1e-9);
        // In no further than 2 s, out no further than the whole.
        v.zoom_about(1e6, 250.0, end, width);
        assert!((v.span_s(width) - MIN_SPAN_S).abs() < 1e-9);
        assert!((v.time_at(250.0) - t).abs() < 1e-9);
        v.zoom_about(1e-6, 250.0, end, width);
        assert_eq!(v, View::fit(end, width));
        // Panning stops at either end.
        v.zoom_about(10.0, 0.0, end, width);
        v.left_s = -5.0;
        v.clamp(end, width);
        assert_eq!(v.left_s, 0.0);
        v.left_s = 1e9;
        v.clamp(end, width);
        assert!((v.time_at(width) - View::room(end)).abs() < 1e-9);
    }

    #[test]
    fn the_rulers_labels_are_80_points_apart_or_more() {
        assert_eq!(tick_step(1000.0), 0.1);
        assert_eq!(tick_step(500.0), 0.2);
        assert_eq!(tick_step(80.0), 1.0);
        assert_eq!(tick_step(79.0), 2.0);
        assert_eq!(tick_step(1.0), 120.0);
        assert_eq!(tick_step(0.01), 1200.0);
        assert_eq!(timecode(75.0, 5.0), "1:15");
        assert_eq!(timecode(75.25, 0.5), "1:15.3");
        assert_eq!(timecode(3599.96, 0.1), "60:00.0");
    }

    #[test]
    fn each_clips_thumbnail_is_its_own() {
        let a = thumbnail(0);
        let b = thumbnail(1);
        assert_eq!(a.size, THUMB);
        assert_ne!(a.pixels, b.pixels);
        assert_eq!(thumbnail(7).pixels, thumbnail(7).pixels);
    }
}
