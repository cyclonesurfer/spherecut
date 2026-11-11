//! The decode workers, one per video stream, and the open worker that
//! starts them (M1 plan sections 8.1, 8.3 and 8.4, DECISIONS D34).
//!
//! **Opening.** The UI's `open` starts an open worker, which picks each
//! stream's decoder ([`crate::decoder::choose`], probing the file and,
//! the first time a stream needs them, running doctor's decode checks),
//! opens the file's audio through the seam slice 6 fills, then clears
//! what the file before left in the cache and the slots, and starts a
//! `decode-v{i}` worker per stream. Each worker opens its own decoder,
//! with the player's shutdown flag as its interrupt.
//!
//! A pair opened locked ([`crate::Player::open_locked`]) has both
//! streams' timing tables read through the backend first, whatever the
//! decoder preference, and its lock group pending in the status. The
//! worker that opens second compares the lenses, under the status lock
//! as it adds its stream's info, and locks the group or refuses it by
//! the field that differs; a member that can't open or decode while the
//! group is pending refuses it with `"open"` (M3 plan section 9).
//!
//! **Requests.** A worker takes one request at a time, the latest
//! winning: the UI stops the ones before with a new [`StopToken`]
//! generation before it sends the next, so a seek or a wait for room in
//! the cache that the next one makes pointless stops at its next frame.
//! A seek first moves the cache's playhead to its time (which frees the
//! frame the seek lands on from waiting for room), then looks in the
//! cache: a run holding the frame shown at the time answers it, and
//! decoding goes on from the end of that run. Otherwise the decoder seeks
//! and its frame starts a new run in the cache. A step follows the run
//! links in the cache, decoding only the frames they don't reach. The
//! frame a request lands on is pinned in the cache until the upload
//! thread has it in a slot ([`crate::FrameCache::pin`]).
//!
//! **Decoding ahead.** Between requests the worker decodes on, frame
//! after frame, into the cache, which holds it back once the window ahead
//! of the playhead is full. While the clock runs and the newest frame
//! decoded is more than two frames behind it, the decoder catches up
//! ([`VideoDecode::catch_up`]): it skips frames nothing refers to and
//! never hands out (so never downloads) a frame before the clock's time;
//! the frames it passed over count as decode-dropped. Once a frame at or
//! after that time is out, catching up ends.
//!
//! **A locked pair** (M3 plan section 9). Once the group locks, the cache
//! runs both members from the group base ([`FrameCache::set_group`]), and
//! a request's playhead holds the base until both have landed it. The
//! members catch up to one time, the group's `until`: a member more than
//! two frames behind the clock raises it to the clock's time, but never
//! past the newest frame the other holds, and each member whose newest
//! frame is before it catches up to it, so that both skip to the same
//! frame. A seek or a step clears it. Decoding ahead, a member waiting
//! for room may be told by the cache's backstop to seek exactly to the
//! trailing member's newest frame instead ([`Insert::Resync`]).
//!
//! **Fallback.** A decoder that goes on in software says so with an
//! event, which the worker puts in the stream's status and the player's
//! events, so that the snapshot and the GUI's notice show it.
//!
//! [`FrameCache::set_group`]: crate::FrameCache::set_group

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use spherecut_core::{Clock, StopToken, Time, may_block};
use spherecut_media::backend::{
    DecodeMode, DecodeStats, DecoderEvent, Exactness, HwKind, HwOutput, MediaBackend, NextFrame,
    SeekMode, SeekOutcome, VideoDecode, VideoFrame, VideoRequest,
};
use spherecut_media::hw::GpuIds;
use spherecut_media::mp4::TrackTiming;
use spherecut_media::{HwDevice, MediaError};
use spherecut_render::vkshare::SharedVideo;

use crate::audio::{AudioOutputFactory, PlayerThreads};
use crate::cache::Insert;
use crate::decoder::{self, DecoderChoice, DecoderPreference, HwSupport, StreamFormat};
use crate::lock::LockSpec;
use crate::perf::{Counter, Stage};
use crate::player::{PlayerError, PlayerEvent};
use crate::shared::{
    Command, Landed, LockGroup, Place, Request, Shared, StreamStatus, Want, frames_between, lock,
};

/// How long an idle worker waits for a command before it looks at the
/// shutdown flag again.
const IDLE_WAIT: Duration = Duration::from_millis(50);

/// How long an open waits for the GUI's hardware checks before it runs
/// them itself. They take a few seconds; a minute means they hung.
const HW_LATER: Duration = Duration::from_secs(60);

/// What an open worker needs.
pub(crate) struct OpenJob {
    pub(crate) shared: Arc<Shared>,
    pub(crate) backend: Arc<dyn MediaBackend>,
    pub(crate) audio: Option<Arc<dyn AudioOutputFactory>>,
    pub(crate) path: PathBuf,
    pub(crate) epoch: u64,
    /// Each stream asked for, by its index in the file (`None` for the
    /// best), with the channel its worker takes requests from.
    pub(crate) streams: Vec<(Option<usize>, Receiver<Command>)>,
    pub(crate) decoder: DecoderPreference,
    /// The open's first request's token.
    pub(crate) token: StopToken,
    /// Stops when a newer open starts, or at shutdown.
    pub(crate) open_token: StopToken,
    /// The clock the player plays on, when the file has no audio.
    pub(crate) clock: Arc<dyn Clock>,
    /// Whether the GPU that renders is a discrete one, and its ids, which
    /// find its render node for doctor's checks.
    pub(crate) discrete: bool,
    pub(crate) gpu_ids: GpuIds,
    pub(crate) render_node: Option<PathBuf>,
    /// VA-API streams decode to DMA-BUFs (slice 5b): zero copy is on and
    /// the device imports them.
    pub(crate) zero_copy: bool,
    /// Vulkan Video streams decode on the renderer's device, shared so
    /// (path B): zero copy is on and the device was made shared.
    pub(crate) shared_video: Option<Arc<SharedVideo>>,
    /// How the two streams are locked, for a pair opened with
    /// [`Player::open_locked`](crate::Player::open_locked).
    pub(crate) lock: Option<LockSpec>,
}

impl OpenJob {
    fn stale(&self) -> bool {
        !self.shared.is_current(self.epoch)
    }

    pub(crate) fn run(self) {
        // A locked pair's tables, read through the backend before the
        // decoders are chosen, whatever the preference: `choose` probes
        // the file only under Auto.
        let group = self
            .lock
            .map(|spec| LockGroup::pending(spec, [0, 1].map(|m| self.timing(m))));
        let choices = self.choose();
        if self.stale() {
            return;
        }
        let audio = match &self.audio {
            Some(factory) => match factory.open(
                &self.path,
                &self.open_token,
                &PlayerThreads::new(
                    self.shared.handles.clone(),
                    self.shared.stop.shutdown_flag(),
                ),
            ) {
                Ok(a) => {
                    if !self.stale() {
                        self.shared.event(match &a {
                            Some(a) => PlayerEvent::AudioOpened {
                                output: a.stats().map(Box::new),
                            },
                            None => PlayerEvent::NoAudio,
                        });
                    }
                    a
                }
                Err(error) => {
                    tracing::warn!("no audio from {}: {error}", self.path.display());
                    if !self.stale() {
                        self.shared.event(PlayerEvent::AudioFailed { error });
                    }
                    None
                }
            },
            None => None,
        };
        let shared = self.shared.clone();
        let _open = lock(&shared.open_lock, "waiting for the open before");
        if self.stale() {
            return;
        }
        shared.cache.clear();
        shared.slots.discard_all();
        let clock = audio.as_ref().map_or(self.clock.clone(), |a| a.clock());
        let old_audio = {
            let mut st = shared.status();
            st.epoch = self.epoch;
            st.path = Some(self.path.clone());
            st.streams = self
                .streams
                .iter()
                .zip(&choices)
                .map(|((asked, _), c)| StreamStatus {
                    asked: *asked,
                    choice: Some(c.why.clone()),
                    ..StreamStatus::default()
                })
                .collect();
            // A locked pair's group, pending until both members open; none
            // for an unlocked open. The cache's group goes under the same
            // lock, so that a worker of the open before, settling its
            // group as this runs, never leaves it set.
            st.lock = group;
            shared.cache.set_group(None);
            std::mem::replace(&mut st.audio, audio)
        };
        // The last open's audio, if this holds it last, outside the lock.
        drop(old_audio);
        shared.live.store(self.epoch, Ordering::Release);
        // An import the driver refused for an open before stays refused:
        // this one's frames are downloaded by the decoder, as without zero
        // copy, not one by one by the upload thread.
        let refused = shared.status().import_refused.clone();
        if let (Some(why), true) = (&refused, self.zero_copy || self.shared_video.is_some()) {
            tracing::info!("zero copy is off for this file: {why}");
        }
        let zero_copy = refused.is_none();
        let dmabuf = zero_copy && self.zero_copy;
        let wants_vulkan = choices.iter().any(|c| {
            matches!(
                c.mode,
                DecodeMode::Hardware {
                    kind: HwKind::Vulkan,
                    device: None,
                    ..
                }
            )
        });
        let vulkan = match &self.shared_video {
            Some(video) if zero_copy && wants_vulkan => shared.vulkan_device(video),
            _ => None,
        };
        for (stream, ((asked, rx), choice)) in self.streams.into_iter().zip(choices).enumerate() {
            tracing::info!(stream, "decoding {}", choice.why);
            let (mode, output) = on_device(choice.mode, dmabuf, vulkan.as_ref());
            let worker = Worker {
                shared: shared.clone(),
                backend: self.backend.clone(),
                epoch: self.epoch,
                stream,
                path: self.path.clone(),
                asked,
                mode,
                output,
                rx,
                clock: clock.clone(),
                token: self.token.clone(),
                pending: None,
                working: None,
                last: None,
                unsaved: false,
                resume: None,
                at_end: false,
                failed: false,
                reported: false,
                catching_up: None,
                seen: DecodeStats::default(),
                spec: self.lock,
            };
            if let Err(e) = shared.spawn(format!("decode-v{stream}"), move || worker.run()) {
                let error = PlayerError::new("thread_failed", format!("starting a decoder: {e}"));
                let group = shared.stream_and_group(
                    self.epoch,
                    stream,
                    |s| s.error = Some(error.clone()),
                    |st| st.member_failed(stream),
                );
                shared.event(PlayerEvent::OpenFailed { stream, error });
                for e in group {
                    shared.event(e);
                }
            }
        }
        shared.repaint();
    }

    /// Player stream `stream`'s timing tables, for its lock group: `None`
    /// for a track with no tables, or whose tables couldn't be read,
    /// which refuses the lock with `"timing"`.
    fn timing(&self, stream: usize) -> Option<TrackTiming> {
        let index = self.streams.get(stream).and_then(|(asked, _)| *asked)?;
        match self.backend.track_timing(&self.path, index) {
            Ok(Some(t)) => Some(t),
            Ok(None) => {
                tracing::info!(stream, "stream {index} has no timing tables to compare");
                None
            }
            Err(e) => {
                tracing::warn!(stream, "can't read stream {index}'s timing tables: {e}");
                None
            }
        }
    }

    /// Each stream's decoder, by the preference.
    fn choose(&self) -> Vec<DecoderChoice> {
        let auto = self.decoder == DecoderPreference::Auto;
        let report = if auto {
            self.backend.probe(&self.path).ok()
        } else {
            None
        };
        let formats: Vec<Option<StreamFormat>> = self
            .streams
            .iter()
            .map(|(asked, _)| report.as_ref().and_then(|r| StreamFormat::of(r, *asked)))
            .collect();
        let checked = formats.iter().flatten().any(|f| f.check_format().is_some());
        let support = if auto && checked {
            self.hw_support()
        } else {
            None
        };
        formats
            .iter()
            .map(|f| decoder::choose(self.decoder, support.as_ref(), f.as_ref(), self.discrete))
            .collect()
    }

    /// Doctor's checks: the player's, the GUI's once they end, or run now
    /// on the GPU's render node; kept for the opens after.
    fn hw_support(&self) -> Option<HwSupport> {
        let mut hw = lock(&self.shared.hw, "waiting for doctor's checks");
        if hw.is_none()
            && !self.stale()
            && let Some(later) = &self.shared.hw_later
        {
            *hw = later.wait(HW_LATER);
            if hw.is_none() {
                tracing::warn!("the GUI's hardware checks didn't come; running them again");
            }
        }
        if hw.is_none() && !self.stale() {
            let node =
                spherecut_media::hw::render_node(Some(&self.gpu_ids), self.render_node.as_deref());
            tracing::info!("checking hardware decoding on {node}");
            *hw = Some(HwSupport::check(&node.path));
        }
        hw.clone()
    }
}

/// A stream's decoder and what it hands frames out as, with zero copy
/// on: VA-API's exported as DMA-BUFs when `dmabuf`, Vulkan Video's left
/// on the renderer's device when FFmpeg's context on it is `vulkan`.
fn on_device(
    mode: DecodeMode,
    dmabuf: bool,
    vulkan: Option<&Arc<HwDevice>>,
) -> (DecodeMode, HwOutput) {
    match (mode, vulkan) {
        (
            DecodeMode::Hardware {
                kind: HwKind::Vulkan,
                device: None,
                fallback,
            },
            Some(dev),
        ) => (
            DecodeMode::Hardware {
                kind: HwKind::Vulkan,
                device: Some(dev.clone()),
                fallback,
            },
            HwOutput::Vulkan,
        ),
        (
            mode @ DecodeMode::Hardware {
                kind: HwKind::VaApi,
                ..
            },
            _,
        ) if dmabuf => (mode, HwOutput::DrmPrime),
        (mode, _) => (mode, HwOutput::Download),
    }
}

/// Why a step of a request didn't get its frame.
enum Halt {
    /// A newer request, or shutdown, stopped it.
    Stopped,
    /// The decoder failed: the stream's status says why.
    Failed,
    /// Decoding ahead, a locked pair's member was told to seek exactly to
    /// this time by the cache's backstop ([`Insert::Resync`]).
    Resync(Time),
}

/// A decode worker.
struct Worker {
    shared: Arc<Shared>,
    backend: Arc<dyn MediaBackend>,
    epoch: u64,
    /// Its place in what was opened, the cache's key for it.
    stream: usize,
    path: PathBuf,
    asked: Option<usize>,
    mode: DecodeMode,
    /// What a device decoder hands frames out as.
    output: HwOutput,
    rx: Receiver<Command>,
    clock: Arc<dyn Clock>,
    /// The latest request's token: decoding ahead after it stops with it.
    token: StopToken,
    /// The request to work on next, and the one being worked on.
    pending: Option<Request>,
    working: Option<u64>,
    /// The frame the decoder handed out last, where it goes on from, and
    /// whether the cache hasn't taken it yet (a newer request stopped its
    /// wait for room).
    last: Option<Arc<VideoFrame>>,
    unsaved: bool,
    /// Decoding goes on from the end of the run holding the frame at
    /// this time, which a request found in the cache.
    resume: Option<Time>,
    at_end: bool,
    failed: bool,
    /// Whether the stream's status holds a failure, which a frame decoded
    /// after it (a seek past it) clears.
    reported: bool,
    /// The clock time the decoder is catching up to.
    catching_up: Option<Time>,
    /// The decoder's numbers when last read.
    seen: DecodeStats,
    /// How the pair is locked, for a pair opened locked.
    spec: Option<LockSpec>,
}

impl Worker {
    fn run(mut self) {
        let Some(mut dec) = self.open() else {
            return;
        };
        loop {
            let idle = self.pending.is_none() && self.is_idle();
            if !self.take_commands(idle) {
                break;
            }
            if let Some(req) = self.pending.take() {
                self.resolve(dec.as_mut(), req);
            } else if !self.is_idle() {
                self.decode_ahead(dec.as_mut());
            }
        }
        tracing::debug!(stream = self.stream, "decode worker ends");
    }

    /// Nothing to decode until a request comes: the end or a failure was
    /// reached, or the latest request was stopped (the next is on its
    /// way).
    fn is_idle(&self) -> bool {
        self.at_end || self.failed || self.last.is_none() || self.token.is_stopped()
    }

    fn open(&mut self) -> Option<Box<dyn VideoDecode>> {
        let req = VideoRequest {
            stream: self.asked,
            decode: self.mode.clone(),
            output: self.output,
            interrupt: self.shared.stop.shutdown_flag(),
            ..VideoRequest::default()
        };
        match self.backend.open_video(&self.path, &req) {
            Ok(mut dec) => {
                let info = dec.info().clone();
                let path = dec.decode_path().clone();
                // With the other member's open, a locked pair's lenses are
                // compared under the same lock.
                let group = self.shared.stream_and_group(
                    self.epoch,
                    self.stream,
                    |s| {
                        s.opened = true;
                        s.info = Some(info.clone());
                        s.decode = Some(path);
                    },
                    |st| {
                        let events = st.settle_lock();
                        // Locked, the cache runs the members from the
                        // group base from now on.
                        if let Some(members) = st.grouped()
                            && self.shared.is_current(self.epoch)
                        {
                            self.shared.cache.set_group(Some(members));
                        }
                        events
                    },
                );
                self.start_audio(info.origin().ok());
                self.take_events(dec.as_mut());
                self.tell(PlayerEvent::Opened {
                    stream: self.stream,
                    info: Box::new(info),
                });
                for e in group {
                    self.tell(e);
                }
                Some(dec)
            }
            Err(e) => {
                if !self.shared.is_shut_down() {
                    let error = PlayerError::from(&e);
                    tracing::warn!(stream = self.stream, "can't open: {e}");
                    let stream = self.stream;
                    let group = self.shared.stream_and_group(
                        self.epoch,
                        stream,
                        |s| s.error = Some(error.clone()),
                        |st| st.member_failed(stream),
                    );
                    self.tell(PlayerEvent::OpenFailed { stream, error });
                    for e in group {
                        self.tell(e);
                    }
                    self.start_audio(None);
                }
                None
            }
        }
    }

    /// The first stream's first frame is clip time 0 for the audio too:
    /// start it decoding from there (only the first open counts).
    fn start_audio(&self, origin: Option<Time>) {
        if self.stream != 0 {
            return;
        }
        let audio = {
            let st = self.shared.status();
            (st.epoch == self.epoch).then(|| st.audio.clone()).flatten()
        };
        if let Some(audio) = audio {
            audio.start(origin);
        }
    }

    /// Take the commands waiting, the latest request winning, waiting for
    /// one first when `wait`. False once the worker should end.
    fn take_commands(&mut self, wait: bool) -> bool {
        if self.shared.is_shut_down() {
            return false;
        }
        let mut got = if wait {
            may_block("waiting for a decode request");
            match self.rx.recv_timeout(IDLE_WAIT) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        } else {
            None
        };
        loop {
            match got.take() {
                Some(Command::Close) => return false,
                Some(Command::Request(r)) => {
                    if self.pending.replace(r.clone()).is_some() {
                        self.count(|s| s.superseded += 1);
                    }
                    self.token = r.token;
                }
                None => {}
            }
            match self.rx.try_recv() {
                Ok(c) => got = Some(c),
                Err(TryRecvError::Empty) => return !self.shared.is_shut_down(),
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn resolve(&mut self, dec: &mut dyn VideoDecode, req: Request) {
        if req.token.is_stopped() {
            self.count(|s| s.superseded += 1);
            return;
        }
        self.count(|s| s.started += 1);
        self.working = Some(req.id);
        self.clear_until();
        let done = match req.want {
            Want::Seek { at, mode } => self.seek(dec, req.id, at, mode, &req.token),
            Want::Step { frames, from, mode } => {
                self.step(dec, req.id, frames, from, mode, &req.token)
            }
        };
        if let Err(Halt::Stopped) = done {
            self.count(|s| s.superseded += 1);
        }
        self.working = None;
    }

    fn seek(
        &mut self,
        dec: &mut dyn VideoDecode,
        id: u64,
        at: Time,
        mode: SeekMode,
        token: &StopToken,
    ) -> Result<(), Halt> {
        let cache = self.shared.cache.clone();
        // The frame the seek lands on is at or behind the playhead then,
        // so it never waits for room.
        self.playhead_for(id, at);
        if let Some(f) = cache.frame_at(self.stream, at) {
            self.count(|s| s.from_cache += 1);
            self.land(id, &f, None, token)?;
            self.resume = Some(f.time);
            return Ok(());
        }
        self.end_catch_up(dec);
        let outcome = dec.seek(at, mode, token);
        self.take_events(dec);
        match outcome {
            Ok(SeekOutcome::Frame {
                frame, exactness, ..
            }) => {
                self.count(|s| s.decoded += 1);
                self.took(dec, frame.clone());
                self.save(token)?;
                self.land(id, &frame, Some(exactness), token)
            }
            Ok(SeekOutcome::Stopped) => Err(Halt::Stopped),
            Err(e) => Err(self.fail(e)),
        }
    }

    fn step(
        &mut self,
        dec: &mut dyn VideoDecode,
        id: u64,
        frames: i32,
        from: Time,
        mode: SeekMode,
        token: &StopToken,
    ) -> Result<(), Halt> {
        let cache = self.shared.cache.clone();
        // From where it starts, as a seek's, so that the frames found on
        // the way never wait for room: the presenter leaves a step's
        // playhead to the worker, and the playhead a seek superseded
        // before it started gave may never have reached the cache.
        self.playhead_for(id, from);
        let start = match mode {
            SeekMode::Exact => cache.get(self.stream, from),
            // Where `seek` would land: the frame shown there if the
            // cache can tell, else the keyframe.
            SeekMode::Keyframe => cache.frame_at(self.stream, from),
        };
        let mut cur = match start {
            Some(f) => f,
            None => self.position(dec, from, mode, token)?,
        };
        for _ in 0..frames.unsigned_abs() {
            if token.is_stopped() {
                return Err(Halt::Stopped);
            }
            let linked = if frames > 0 {
                cache.next_after(self.stream, cur.time)
            } else {
                cache.prev_before(self.stream, cur.time)
            };
            let next = match linked {
                Some(f) => Some(f),
                None if frames > 0 => self.decode_after(dec, &cur, token)?,
                None => self.decode_before(dec, &cur, token)?,
            };
            let Some(next) = next else {
                break;
            };
            cur = next;
            self.playhead_for(id, cur.time);
        }
        self.land(id, &cur, None, token)?;
        self.resume = Some(cur.time);
        Ok(())
    }

    /// The decoder at the frame a seek to `at` by `mode` lands on: where
    /// it is, or sought there.
    fn position(
        &mut self,
        dec: &mut dyn VideoDecode,
        at: Time,
        mode: SeekMode,
        token: &StopToken,
    ) -> Result<Arc<VideoFrame>, Halt> {
        if mode == SeekMode::Exact
            && let Some(last) = &self.last
            && last.time == at
        {
            return Ok(last.clone());
        }
        self.end_catch_up(dec);
        let outcome = dec.seek(at, mode, token);
        self.take_events(dec);
        match outcome {
            Ok(SeekOutcome::Frame { frame, .. }) => {
                self.took(dec, frame.clone());
                self.save(token)?;
                Ok(frame)
            }
            Ok(SeekOutcome::Stopped) => Err(Halt::Stopped),
            Err(e) => Err(self.fail(e)),
        }
    }

    /// The frame after `cur`, decoded; `None` at the end.
    fn decode_after(
        &mut self,
        dec: &mut dyn VideoDecode,
        cur: &VideoFrame,
        token: &StopToken,
    ) -> Result<Option<Arc<VideoFrame>>, Halt> {
        self.position(dec, cur.time, SeekMode::Exact, token)?;
        if self.at_end {
            return Ok(None);
        }
        self.end_catch_up(dec);
        self.next(dec, token)
    }

    /// The frame before `cur`, decoded: the one shown a tick before it;
    /// `None` at the first.
    fn decode_before(
        &mut self,
        dec: &mut dyn VideoDecode,
        cur: &VideoFrame,
        token: &StopToken,
    ) -> Result<Option<Arc<VideoFrame>>, Halt> {
        let tick = Time::from_pts(1, dec.info().time_base);
        let Some(before) = tick.ok().and_then(|t| cur.time.checked_sub(t).ok()) else {
            return Ok(None);
        };
        let f = self.position(dec, before, SeekMode::Exact, token)?;
        Ok((f.time < cur.time).then_some(f))
    }

    /// The decoder's next frame, into the cache.
    fn next(
        &mut self,
        dec: &mut dyn VideoDecode,
        token: &StopToken,
    ) -> Result<Option<Arc<VideoFrame>>, Halt> {
        let got = dec.next_frame(token);
        self.take_events(dec);
        match got {
            Ok(NextFrame::Frame(f)) => {
                if self.catching_up.is_some()
                    && let Some(prev) = &self.last
                {
                    let skipped = frames_between(&Place::of_frame(prev), &Place::of_frame(&f));
                    self.shared.perf.add(Counter::DecodeDropped, skipped);
                }
                if self.catching_up.is_some_and(|until| f.time >= until) {
                    self.end_catch_up(dec);
                }
                self.took(dec, f.clone());
                self.save(token)?;
                Ok(Some(f))
            }
            Ok(NextFrame::End) => {
                self.at_end = true;
                let last = self.last.as_ref().map(|l| l.time);
                let end = self
                    .last
                    .as_ref()
                    .and_then(|l| l.time.checked_add(l.duration?).ok());
                self.shared.stream(self.epoch, self.stream, |s| {
                    s.end = end;
                    s.last = last;
                });
                self.tell(PlayerEvent::StreamEnded {
                    stream: self.stream,
                });
                Ok(None)
            }
            Ok(NextFrame::Stopped) => Err(Halt::Stopped),
            Err(e) => Err(self.fail(e)),
        }
    }

    fn decode_ahead(&mut self, dec: &mut dyn VideoDecode) {
        let token = self.token.clone();
        let mut went = self.go_ahead(dec, &token);
        while let Err(Halt::Resync(at)) = went {
            went = self.resync(dec, at, &token);
        }
    }

    /// Decode the next frame ahead into the cache, going on first from
    /// where a request left off.
    fn go_ahead(&mut self, dec: &mut dyn VideoDecode, token: &StopToken) -> Result<(), Halt> {
        let cache = self.shared.cache.clone();
        if let Some(t) = self.resume.take() {
            // A frame the cache had no room for is passed over unless it
            // fits now: the run in the cache says where to go on from.
            if self.unsaved
                && let Some(last) = self.last.clone()
            {
                if cache.try_insert_until(self.stream, last, token) == Insert::Stopped {
                    self.resume = Some(t);
                    return Err(Halt::Stopped);
                }
                self.unsaved = false;
            }
            if let Some(end) = cache.run_end(self.stream, t) {
                match self.position(dec, end.time, SeekMode::Exact, token) {
                    Ok(_) => {}
                    Err(Halt::Stopped) => {
                        self.resume = Some(t);
                        return Err(Halt::Stopped);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        self.save(token)?;
        if self.is_idle() {
            return Ok(());
        }
        self.catch_up(dec);
        self.next(dec, token).map(|_| ())
    }

    /// The cache's backstop held a locked pair's base at `at`, the
    /// trailing member's newest frame: put this member's frame there in,
    /// seeking exactly to it unless it is the frame that waited for room
    /// (which is passed over otherwise), and decode on from it. The
    /// clock stays where it is.
    fn resync(
        &mut self,
        dec: &mut dyn VideoDecode,
        at: Time,
        token: &StopToken,
    ) -> Result<(), Halt> {
        self.resume = None;
        let went = if self.last.as_ref().is_some_and(|l| l.time == at) {
            self.save(token)
        } else {
            self.unsaved = false;
            self.position(dec, at, SeekMode::Exact, token).map(|_| ())
        };
        // Done or failed, the backstop no longer waits for this member; a
        // newer request that stopped it holds the base instead.
        if !matches!(went, Err(Halt::Stopped)) {
            self.shared.cache.landed(self.stream, None);
        }
        went
    }

    /// Start or go on catching up when the newest frame is more than two
    /// frames behind the running clock.
    fn catch_up(&mut self, dec: &mut dyn VideoDecode) {
        if !self.clock.is_running() {
            self.end_catch_up(dec);
            return;
        }
        let Some(last) = &self.last else {
            return;
        };
        let newest = last.time;
        let Some(frame) = last
            .duration
            .or_else(|| dec.info().frame_rate().map(|r| r.frame_duration()))
        else {
            return;
        };
        let now = self.clock.time_at(Instant::now());
        let behind = now
            .checked_sub(newest)
            .ok()
            .zip(frame.checked_add(frame).ok())
            .is_some_and(|(lag, two)| lag > two);
        let until = match self.group_until(now, behind) {
            // A locked pair's member catches up only to the group's time,
            // whichever member raised it, so that both skip to one frame.
            Some(until) => until.filter(|&u| newest < u && self.catching_up != Some(u)),
            None => (behind && self.catching_up.is_none_or(|until| now > until)).then_some(now),
        };
        if let Some(until) = until {
            dec.catch_up(Some(until));
            if self.catching_up.replace(until).is_none() {
                self.shared
                    .stream(self.epoch, self.stream, |s| s.catching_up = true);
            }
        }
    }

    /// A locked pair's catch-up time, given the clock's time `now` and
    /// whether this member's newest frame is more than two frames behind
    /// it: the group's `until`, which a member behind raises to `now`, but
    /// never past the newest frame every other member holds. `None`
    /// unless the pair is locked with its group base on: the stream then
    /// catches up on its own.
    fn group_until(&self, now: Time, behind: bool) -> Option<Option<Time>> {
        if !self.spec.is_some_and(|s| !s.test.no_group_base) {
            return None;
        }
        let cache = &self.shared.cache;
        let mut st = self.shared.status();
        if st.epoch != self.epoch {
            return None;
        }
        let members = st.grouped().filter(|m| m.contains(&self.stream))?;
        // Each other member's newest frame in the cache, at or before
        // the newest it handed out (frames a seek back left ahead lie
        // past it).
        let held = members
            .iter()
            .filter(|&&m| m != self.stream)
            .map(|&m| {
                let newest = st.streams.get(m)?.newest?;
                Some(cache.at_or_before(m, newest)?.time)
            })
            .collect::<Option<Vec<Time>>>()
            .and_then(|h| h.into_iter().min());
        let g = st.lock.as_mut()?;
        if behind
            && let Some(held) = held
            && g.until.is_none_or(|u| u < now.min(held))
        {
            g.until = Some(now.min(held));
        }
        Some(g.until)
    }

    /// A seek or a step clears a locked pair's catch-up time.
    fn clear_until(&self) {
        if !self.spec.is_some_and(|s| !s.test.no_group_base) {
            return;
        }
        let mut st = self.shared.status();
        if st.epoch == self.epoch
            && let Some(g) = st.lock.as_mut()
        {
            g.until = None;
        }
    }

    /// Move the cache's playhead to `at` for request `id`: as a seek's,
    /// which a locked pair's group base follows until every member has
    /// landed it, unless the test's switch keeps the old common time.
    fn playhead_for(&self, id: u64, at: Time) {
        let cache = &self.shared.cache;
        if self.spec.is_some_and(|s| s.test.old_base_in_seeks) {
            cache.set_playhead(at);
        } else {
            cache.seek_playhead(self.stream, id, at);
        }
    }

    fn end_catch_up(&mut self, dec: &mut dyn VideoDecode) {
        if self.catching_up.take().is_some() {
            dec.catch_up(None);
            self.shared
                .stream(self.epoch, self.stream, |s| s.catching_up = false);
        }
    }

    /// The decoder handed out `f`: counted in the stream's status, with
    /// the frames decoded since the last, which clears a failure it
    /// reported.
    fn took(&mut self, dec: &mut dyn VideoDecode, f: Arc<VideoFrame>) {
        let time = f.time;
        self.last = Some(f);
        self.unsaved = true;
        self.at_end = false;
        self.failed = false;
        let stats = dec.stats();
        let perf = &self.shared.perf;
        let decoded = stats
            .frames_decoded
            .saturating_sub(self.seen.frames_decoded);
        perf.add(Counter::FramesDecoded, decoded);
        perf.record(Stage::Decode, stats.last_decode_ms as f32);
        if stats.last_download_ms > 0.0 {
            perf.record(Stage::Download, stats.last_download_ms as f32);
        }
        if stats.last_convert_ms > 0.0 {
            perf.record(Stage::Convert, stats.last_convert_ms as f32);
        }
        self.seen = stats;
        let cleared = std::mem::take(&mut self.reported);
        self.shared.stream(self.epoch, self.stream, |s| {
            s.handed_out += 1;
            s.newest = Some(time);
            s.decoded += decoded;
            if cleared {
                s.error = None;
                s.failed_request = None;
            }
        });
    }

    /// Put the frame last handed out in the cache, waiting for room;
    /// stopped, it stays to put in later. Decoding ahead, a locked pair's
    /// member may be told to seek to the backstop's time instead.
    fn save(&mut self, token: &StopToken) -> Result<(), Halt> {
        let Some(f) = self.last.clone().filter(|_| self.unsaved) else {
            return Ok(());
        };
        let cache = &self.shared.cache;
        let got = if self.working.is_none() && !self.spec.is_some_and(|s| s.test.no_resync) {
            cache.insert_ahead(self.stream, f, token)
        } else {
            cache.insert(self.stream, f, token)
        };
        match got {
            Insert::Stopped => Err(Halt::Stopped),
            Insert::Resync { at } => Err(Halt::Resync(at)),
            _ => {
                self.unsaved = false;
                Ok(())
            }
        }
    }

    /// Tell the presenter that request `id` found `f`, pinned in the cache
    /// while `id` is the target (until the presenter has every stream's
    /// up, or a newer request comes): the frames decoded on from a
    /// keyframe behind the time asked for, or another stream's worker
    /// moving the playhead, would otherwise have it evicted first.
    fn land(
        &self,
        id: u64,
        f: &Arc<VideoFrame>,
        exactness: Option<Exactness>,
        token: &StopToken,
    ) -> Result<(), Halt> {
        if self.shared.cache.pin(self.stream, f.clone(), token) == Insert::Stopped {
            return Err(Halt::Stopped);
        }
        self.shared.cache.landed(self.stream, Some(id));
        let landed = Landed {
            id,
            time: f.time,
            pts: f.pts,
            exactness,
        };
        self.shared
            .stream(self.epoch, self.stream, |s| s.landed = Some(landed));
        // The upload thread looks again, for the frame to upload first.
        self.shared.cache.wake();
        self.shared.repaint();
        Ok(())
    }

    /// The decoder failed with `e`, after any fallback to software: say
    /// so, unless it was shut down. A locked pair that hasn't formed yet
    /// is refused with `"open"`.
    fn fail(&mut self, e: MediaError) -> Halt {
        self.failed = true;
        if self.shared.is_shut_down() {
            return Halt::Stopped;
        }
        tracing::warn!(stream = self.stream, "decoding failed: {e}");
        self.reported = true;
        let error = PlayerError::from(&e);
        let working = self.working;
        let stream = self.stream;
        let group = self.shared.stream_and_group(
            self.epoch,
            stream,
            |s| {
                s.error = Some(error.clone());
                s.failed_request = working;
            },
            |st| st.member_failed(stream),
        );
        // A locked pair's base no longer waits for this member's landing.
        if let Some(id) = working {
            self.shared.cache.landed(stream, Some(id));
        }
        self.tell(PlayerEvent::DecodeFailed { stream, error });
        for e in group {
            self.tell(e);
        }
        Halt::Failed
    }

    fn take_events(&mut self, dec: &mut dyn VideoDecode) {
        for e in dec.take_events() {
            match e {
                DecoderEvent::FellBack { from, reason } => {
                    tracing::warn!(
                        stream = self.stream,
                        "{from} decoding goes on in software: {reason}"
                    );
                    let path = dec.decode_path().clone();
                    let fallback = (from, reason.clone());
                    self.shared.stream(self.epoch, self.stream, |s| {
                        s.fallback = Some(fallback);
                        s.decode = Some(path);
                    });
                    self.tell(PlayerEvent::FellBack {
                        stream: self.stream,
                        from,
                        reason,
                    });
                }
                event => self.tell(PlayerEvent::Decoder {
                    stream: self.stream,
                    event,
                }),
            }
        }
    }

    fn count(&self, f: impl FnOnce(&mut crate::shared::SeekCounts)) {
        self.shared
            .stream(self.epoch, self.stream, |s| f(&mut s.seeks));
    }

    /// An event, if this open is still the latest.
    fn tell(&self, e: PlayerEvent) {
        if self.shared.is_current(self.epoch) {
            self.shared.event(e);
        }
    }
}
