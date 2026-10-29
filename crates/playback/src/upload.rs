//! The upload thread (M1 plan sections 8.1 and 8.3, DECISIONS D34): it
//! puts the frames the presenter will want next into GPU slots, and
//! keeps the cache's budget.
//!
//! **What it uploads**, per stream, in order, as far as its share of the
//! slots (the active slots over the streams, at least 2):
//!
//! - after a seek or a step, nothing until the request lands (the frame
//!   on screen stays up meanwhile), then the frame it landed on, and
//!   while any stream's frame for it isn't in a slot, nothing more: the
//!   frames a stream that landed first would want next must not take
//!   the slot another stream's needs. A stream that failed the request
//!   wants nothing;
//! - otherwise the frame at the playhead, the latest at or before it,
//!   which is the one shown or about to be;
//! - while playing, the frames after it, in order;
//! - while paused, the frame after it and the one before (by the run
//!   links, so the one adjacent in the stream), so that a step either
//!   way is instant, then more frames after.
//!
//! A Ready slot holding a frame no longer wanted (passed by the
//! playhead, or of a run a seek left) is freed for the next; a Shown one
//! never is. One frame is uploaded per pass. With nothing to do it waits
//! for the cache to change, 20 ms at most, so that it looks at slots a
//! draw has let go of, its commands and the shutdown flag.
//!
//! **GPU memory** (risk 11): a slot is made the first time a frame of
//! its shape goes in, inside the out-of-memory error scope
//! [`GpuFrame::new`] pushes. A slot the GPU has no memory for is dropped
//! for good (Dead), and playback goes on with one fewer, with an event.
//!
//! **The budget**: set when the thread starts, and rechecked every 5 s
//! against the memory available, with the cache's own bytes counted as
//! free ([`crate::budget::recheck`]).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use spherecut_core::video::VideoPlanes;
use spherecut_core::{Time, may_block};
use spherecut_media::backend::{VideoFrame, VideoStreamInfo};
use spherecut_media::spherical::StereoMode;
use spherecut_render::{Gpu, GpuFrame, Placement, RenderError, SlotShape, Stereo, Uploader};

use crate::budget::{self, CacheBudget};
use crate::cache::WAIT_SLICE;
use crate::perf::{Counter, Stage};
use crate::player::{PlayerError, PlayerEvent};
use crate::shared::{BudgetStatus, Landed, Shared, time_of};
use crate::slots::{SlotFrame, SlotState};

/// How often the budget is rechecked against the memory available.
pub const BUDGET_RECHECK: Duration = Duration::from_secs(5);

/// How often the cache's numbers are read for the counters and the "too
/// small" notice.
const STATS_EVERY: Duration = Duration::from_millis(250);

/// How long the thread waits at shutdown for the GPU to finish its work.
const FINISH: Duration = Duration::from_secs(1);

/// What the UI tells the upload thread.
#[derive(Debug)]
pub(crate) enum UploadCommand {
    Budget(CacheBudget),
    Ahead(Duration),
}

pub(crate) struct UploadThread {
    pub(crate) shared: Arc<Shared>,
    pub(crate) gpu: Gpu,
    pub(crate) rx: Receiver<UploadCommand>,
    pub(crate) configured: CacheBudget,
    /// Reads the memory available: [`budget::read_auto_budget`], or a
    /// test's reading.
    pub(crate) read: fn() -> budget::AutoBudget,
}

/// What a pass reads of a stream's status.
struct StreamPass {
    landed: Option<Landed>,
    /// It failed the target's request, or can't open: it shows no new
    /// frame for it.
    out: bool,
}

/// The upload thread's own state.
struct State {
    uploader: Uploader,
    /// Frames that failed to upload, which aren't tried again.
    failed: HashSet<(u64, usize, Time)>,
    /// The open the "too small" notice was given for.
    too_small: u64,
    /// Per stream, the request whose landing was last unpinned.
    unpinned: Vec<u64>,
    waits: u64,
    budget_at: Instant,
    stats_at: Instant,
    /// The GPU failed: nothing more is uploaded.
    lost: bool,
}

impl UploadThread {
    pub(crate) fn run(mut self) {
        let mut st = State {
            uploader: Uploader::new(&self.gpu),
            failed: HashSet::new(),
            too_small: 0,
            unpinned: Vec::new(),
            waits: 0,
            budget_at: Instant::now(),
            stats_at: Instant::now(),
            lost: false,
        };
        self.recheck(&mut st);
        while !self.shared.is_shut_down() {
            if !self.commands(&mut st) {
                break;
            }
            if st.budget_at.elapsed() >= BUDGET_RECHECK {
                self.recheck(&mut st);
            }
            if st.stats_at.elapsed() >= STATS_EVERY {
                self.cache_stats(&mut st);
            }
            let seen = self.shared.cache.generation();
            let did = !st.lost && self.pass(&mut st);
            if !did {
                self.shared.cache.wait_for_change(seen, WAIT_SLICE);
            }
        }
        // The current submit has returned; let the GPU finish with it.
        may_block("waiting for the GPU at shutdown");
        if let Err(e) = self.gpu.device.poll(wgpu_wait(FINISH)) {
            tracing::debug!("the GPU's last work at shutdown: {e}");
        }
        self.shared.cache.close();
        tracing::debug!("upload thread ends");
    }

    /// Take the UI's commands. False once the player is gone.
    fn commands(&mut self, st: &mut State) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(UploadCommand::Budget(b)) => {
                    self.configured = b;
                    self.recheck(st);
                }
                Ok(UploadCommand::Ahead(d)) => self.shared.cache.set_ahead(time_of(d)),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Set the budget from the memory available now: what the setting
    /// stands for, and with memory short, less, below an explicit size
    /// too ([`budget::recheck`]).
    fn recheck(&self, st: &mut State) {
        st.budget_at = Instant::now();
        let now = (self.read)();
        let held = self.shared.cache.stats().bytes;
        let bytes = budget::recheck(self.configured, &now, held);
        let mut status = self.shared.status();
        if status.budget.is_none_or(|b| b.bytes != bytes) {
            tracing::info!("frame cache budget: {} MiB", bytes >> 20);
            self.shared.cache.set_budget(bytes);
        }
        status.budget = Some(BudgetStatus {
            configured: self.configured,
            bytes,
            auto: Some(now),
        });
    }

    /// The cache's numbers: its waits for the counter, and the notice
    /// when the budget holds too few frames. And the GPU memory of slots
    /// the setting no longer uses, once nothing holds them.
    fn cache_stats(&self, st: &mut State) {
        st.stats_at = Instant::now();
        self.shared.slots.trim();
        let stats = self.shared.cache.stats();
        self.shared
            .perf
            .add(Counter::CacheWaits, stats.waits.saturating_sub(st.waits));
        st.waits = stats.waits;
        let live = self.shared.live.load(Ordering::Acquire);
        if stats.too_small && st.too_small != live && self.shared.is_current(live) {
            st.too_small = live;
            self.shared.event(PlayerEvent::CacheTooSmall {
                budget: stats.budget,
                frame_bytes: stats.frame_bytes,
            });
        }
    }

    /// Free what isn't wanted and upload the next frame that is. Whether
    /// it did anything.
    fn pass(&self, st: &mut State) -> bool {
        let shared = &self.shared;
        let epoch = shared.live.load(Ordering::Acquire);
        let target = shared.target.load(Ordering::Acquire);
        let (streams, placements): (Vec<StreamPass>, Vec<Placement>) = {
            let status = shared.status();
            if status.epoch != epoch {
                return false;
            }
            status
                .streams
                .iter()
                .map(|s| {
                    let pass = StreamPass {
                        landed: s.landed,
                        out: target != 0
                            && (s.failed_request == Some(target)
                                || (!s.opened && s.error.is_some())),
                    };
                    (pass, placement_of(s.info.as_ref()))
                })
                .unzip()
        };
        let slots = &shared.slots;
        let mut did = false;
        for v in slots.views() {
            if v.state == SlotState::Ready && v.frame.epoch != epoch {
                did |= slots.discard(&v);
            }
        }
        let n = streams.len();
        if n == 0 {
            return did;
        }
        let playing = shared.playing.load(Ordering::Acquire);
        let views = slots.views();
        let in_slot = |i: usize, t: Time| {
            views
                .iter()
                .any(|v| v.frame.epoch == epoch && v.frame.stream == i && v.frame.time == t)
        };
        // A landing in a slot needs the cache no more: the presenter
        // shows it from there.
        st.unpinned.resize(n, 0);
        for (i, s) in streams.iter().enumerate() {
            if let Some(l) = s.landed
                && st.unpinned[i] != l.id
                && in_slot(i, l.time)
            {
                shared.cache.unpin(i, l.time);
                st.unpinned[i] = l.id;
            }
        }
        // While a stream's frame for the target isn't in a slot, every
        // stream wants its own alone: the frames a stream that landed
        // first would want after its own must leave the slot the others
        // need free (each stream's frame on screen stays up meanwhile).
        let waiting = target != 0
            && streams.iter().enumerate().any(|(i, s)| {
                !s.out
                    && !s
                        .landed
                        .is_some_and(|l| l.id == target && in_slot(i, l.time))
            });
        let quota = if waiting {
            1
        } else {
            (slots.stats().active / n).max(2)
        };
        let playhead = shared.cache.playhead();
        let wanted: Vec<Option<Vec<Arc<VideoFrame>>>> = streams
            .iter()
            .enumerate()
            .map(|(i, s)| self.wanted(i, target, s, playing, playhead, quota))
            .collect();
        let views = slots.views();
        for v in &views {
            if v.state == SlotState::Ready
                && v.frame.epoch == epoch
                && let Some(Some(w)) = wanted.get(v.frame.stream)
                && !w.iter().any(|f| f.time == v.frame.time)
            {
                did |= slots.discard(v);
            }
        }
        let views = slots.views();
        let have = |i: usize, t: Time| {
            views
                .iter()
                .any(|v| v.frame.epoch == epoch && v.frame.stream == i && v.frame.time == t)
        };
        for rank in 0..quota {
            for (i, w) in wanted.iter().enumerate() {
                let Some(f) = w.as_ref().and_then(|w| w.get(rank)) else {
                    continue;
                };
                if have(i, f.time) || st.failed.contains(&(epoch, i, f.time)) {
                    continue;
                }
                let placement = placements.get(i).copied().unwrap_or(Placement::FULL);
                return self.upload(st, epoch, i, f, &placement) || did;
            }
        }
        did
    }

    /// The frames of stream `i` to have in slots, most wanted first;
    /// `None` to leave its slots as they are (its request hasn't landed).
    /// A stream out of the target (it failed it) wants none.
    fn wanted(
        &self,
        i: usize,
        target: u64,
        stream: &StreamPass,
        playing: bool,
        playhead: Time,
        quota: usize,
    ) -> Option<Vec<Arc<VideoFrame>>> {
        let cache = &self.shared.cache;
        let base = if target != 0 {
            if stream.out {
                return Some(Vec::new());
            }
            match stream.landed {
                Some(l) if l.id == target => cache.get(i, l.time),
                _ => return None,
            }
        } else {
            cache
                .at_or_before(i, playhead)
                .or_else(|| cache.following(i, playhead).map(|(f, _)| f))
        }?;
        let mut out = vec![base.clone()];
        let mut ahead = base.clone();
        let mut more = |out: &mut Vec<Arc<VideoFrame>>| {
            let next = cache.following(i, ahead.time).map(|(f, _)| f);
            if let Some(f) = &next {
                ahead = f.clone();
                out.push(f.clone());
            }
            next.is_some()
        };
        if !playing {
            more(&mut out);
            if out.len() < quota
                && let Some(p) = cache.prev_before(i, base.time)
            {
                out.push(p);
            }
        }
        while out.len() < quota && more(&mut out) {}
        out.truncate(quota);
        Some(out)
    }

    /// Upload frame `f` of stream `i` into a free slot: whether it did
    /// anything (false when no slot is free).
    fn upload(
        &self,
        st: &mut State,
        epoch: u64,
        i: usize,
        f: &Arc<VideoFrame>,
        placement: &Placement,
    ) -> bool {
        let shared = &self.shared;
        let slots = &shared.slots;
        let shape = match SlotShape::of(f.layout(), placement.shown(f.size())) {
            Ok(s) => s,
            Err(e) => {
                self.upload_failed(st, epoch, i, f, &e);
                return true;
            }
        };
        let Some(claim) = slots.claim() else {
            return false;
        };
        let gpu_frame = match slots.frame_of(&claim) {
            Some(g) if g.shape().fits(&shape) => g,
            _ => match GpuFrame::new(&self.gpu, &shape) {
                Ok(g) => {
                    let g = Arc::new(g);
                    slots.replace(&claim, g.clone());
                    g
                }
                Err(e @ RenderError::OutOfMemory(_)) => {
                    let slot = claim.index;
                    slots.kill(claim);
                    tracing::warn!("GPU slot {slot} dropped: {e}");
                    shared.event(PlayerEvent::SlotDropped {
                        slot,
                        error: PlayerError::from(&e),
                    });
                    return true;
                }
                Err(e) => {
                    slots.abandon(claim);
                    self.render_failed(st, epoch, i, f, e);
                    return true;
                }
            },
        };
        match st
            .uploader
            .upload_placed(&self.gpu, f.as_ref(), placement, &gpu_frame)
        {
            Ok(stats) => {
                drop(gpu_frame);
                slots.publish(
                    claim,
                    &SlotFrame {
                        stream: i,
                        epoch,
                        time: f.time,
                        pts: f.pts,
                        run: f.run,
                        seq: f.seq,
                        duration: f.duration,
                    },
                );
                shared.perf.record(Stage::UploadCpu, stats.cpu_ms);
                if let Some(ms) = stats.gpu_ms {
                    shared.perf.record(Stage::UploadGpu, ms);
                }
                shared.perf.add(Counter::FramesUploaded, 1);
                shared.repaint();
            }
            Err(e @ RenderError::OutOfMemory(_)) => {
                drop(gpu_frame);
                let slot = claim.index;
                slots.kill(claim);
                tracing::warn!("GPU slot {slot} dropped: {e}");
                shared.event(PlayerEvent::SlotDropped {
                    slot,
                    error: PlayerError::from(&e),
                });
            }
            Err(e) => {
                drop(gpu_frame);
                slots.abandon(claim);
                self.render_failed(st, epoch, i, f, e);
            }
        }
        true
    }

    /// An upload failed with `e`: the GPU, which stops uploading, or the
    /// frame, which isn't tried again.
    fn render_failed(&self, st: &mut State, epoch: u64, i: usize, f: &VideoFrame, e: RenderError) {
        if matches!(e, RenderError::Device(_)) {
            tracing::error!("the GPU failed: {e}");
            st.lost = true;
            let error = PlayerError::from(&e);
            self.shared.status().gpu_error = Some(error.clone());
            self.shared.event(PlayerEvent::GpuFailed { error });
        } else {
            self.upload_failed(st, epoch, i, f, &e);
        }
    }

    fn upload_failed(&self, st: &mut State, epoch: u64, i: usize, f: &VideoFrame, e: &RenderError) {
        tracing::warn!(stream = i, "can't upload the frame at {:?}: {e}", f.time);
        st.failed.retain(|&(ep, _, _)| ep == epoch);
        st.failed.insert((epoch, i, f.time));
        if self.shared.is_current(epoch) {
            self.shared.event(PlayerEvent::UploadFailed {
                stream: i,
                time: f.time,
                error: PlayerError::from(e),
            });
        }
    }
}

/// Where a stream's picture sits on the sphere, from its mapping, as
/// `spherecut frame` places it (an unknown stereo layout as mono).
pub(crate) fn placement_of(info: Option<&VideoStreamInfo>) -> Placement {
    let Some(info) = info else {
        return Placement::FULL;
    };
    let m = &info.mapping;
    let stereo = match m.stereo {
        StereoMode::TopBottom => Stereo::TopBottom,
        StereoMode::LeftRight => Stereo::LeftRight,
        _ => Stereo::Mono,
    };
    let [u0, v0, u1, v1] = m.cover;
    Placement {
        left: u0,
        top: v0,
        right: 1.0 - u1,
        bottom: 1.0 - v1,
        stereo,
    }
}

fn wgpu_wait(timeout: Duration) -> spherecut_render::wgpu::PollType {
    spherecut_render::wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(timeout),
    }
}
