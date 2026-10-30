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
//! - while playing, the frames after it, in order, as far as they follow
//!   on: linked to the one before, or starting where it ends. A later
//!   frame past a gap (one a seek back left ahead of the playhead) waits
//!   until the frames between are in;
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
//! **Frames on the device** (slice 5b): a frame a VA-API decoder
//! exported as DMA-BUFs has its planes imported and copied on the GPU
//! ([`spherecut_render::dmabuf`]), and is held until the GPU has read
//! them (`Queue::on_submitted_work_done`), since its decoder writes the
//! surface again once it is let go. A Vulkan Video frame decoded on the
//! renderer's own device (path B) is copied out of FFmpeg's image into a
//! buffer by the device's copier ([`spherecut_render::vkshare`]), waited
//! for here, and let go at once; the slot is filled from the buffer,
//! which goes back to the copier once the GPU has read it. When the
//! import or the copy is refused, the frame is downloaded here and
//! uploaded as bytes, and the reason, kept in the status, turns zero copy
//! off for the opens after.
//!
//! **GPU memory** (risk 11): a slot is made the first time a frame of
//! its shape goes in, inside the out-of-memory error scope
//! [`GpuFrame::new`] pushes. A slot the GPU has no memory for is dropped
//! for good (Dead), and playback goes on with one fewer, with an event.
//!
//! **The budget**: set when the thread starts, and rechecked every 5 s
//! against the memory available, with the cache's own bytes counted as
//! free ([`crate::budget::recheck`]).
//!
//! **The memory**, for the overlay: the process's resident set and minor
//! faults, and wgpu's allocator report, read once a second.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use spherecut_core::video::VideoPlanes;
use spherecut_core::{Time, diag, may_block};
use spherecut_media::backend::{VideoFrame, VideoStreamInfo};
use spherecut_media::spherical::StereoMode;
use spherecut_render::dmabuf::{self, Importer};
use spherecut_render::{
    Gpu, GpuFrame, Placement, RenderError, SlotShape, Stereo, UploadPath, UploadStats, Uploader,
    upload,
};

use crate::budget::{self, CacheBudget};
use crate::cache::WAIT_SLICE;
use crate::perf::{self, Counter, MemoryStats, Stage};
use crate::player::{PlayerError, PlayerEvent};
use crate::shared::{BudgetStatus, Landed, Place, Shared, frames_between, time_of};
use crate::slots::{SlotFrame, SlotState};

/// How often the budget is rechecked against the memory available.
pub const BUDGET_RECHECK: Duration = Duration::from_secs(5);

/// How often the cache's numbers are read for the counters and the "too
/// small" notice.
const STATS_EVERY: Duration = Duration::from_millis(250);

/// How often the memory is read for the overlay.
const MEMORY_EVERY: Duration = Duration::from_secs(1);

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
    importer: Importer,
    /// Frames that failed to upload, which aren't tried again.
    failed: HashSet<(u64, usize, Time)>,
    /// The open the "too small" notice was given for.
    too_small: u64,
    /// Per stream, the request whose landing was last unpinned.
    unpinned: Vec<u64>,
    waits: u64,
    budget_at: Instant,
    stats_at: Instant,
    memory_at: Instant,
    /// The minor faults at the last reading, and when.
    faults: Option<(Instant, u64)>,
    /// The GPU failed: nothing more is uploaded.
    lost: bool,
}

impl UploadThread {
    pub(crate) fn run(mut self) {
        let mut st = State {
            // The belt, not `write_texture`: wgpu holds the queue's
            // pending writes locked through `write_texture`'s copy of a
            // frame, which the UI thread's own writes and submit then
            // wait for (D31, odin's GUI in slice 9).
            uploader: Uploader::with_path(&self.gpu, UploadPath::StagingBelt),
            importer: Importer::new(),
            failed: HashSet::new(),
            too_small: 0,
            unpinned: Vec::new(),
            waits: 0,
            budget_at: Instant::now(),
            stats_at: Instant::now(),
            memory_at: Instant::now(),
            faults: None,
            lost: false,
        };
        self.recheck(&mut st);
        self.memory(&mut st);
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
            if st.memory_at.elapsed() >= MEMORY_EVERY {
                self.memory(&mut st);
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

    /// Read the process's memory and the GPU's into the status.
    fn memory(&self, st: &mut State) {
        let now = Instant::now();
        st.memory_at = now;
        let read = |path: &str| std::fs::read_to_string(path).ok();
        let faults = read("/proc/self/stat")
            .as_deref()
            .and_then(perf::minor_faults_of);
        let per_s = st.faults.zip(faults).and_then(|((at, before), n)| {
            let dt = now.duration_since(at).as_secs_f64();
            (dt > 0.0).then(|| n.saturating_sub(before) as f64 / dt)
        });
        if let Some(n) = faults {
            st.faults = Some((now, n));
        }
        let report = self.gpu.device.generate_allocator_report();
        let memory = MemoryStats {
            rss: read("/proc/self/status").as_deref().and_then(perf::rss_of),
            minor_faults: faults,
            minor_faults_per_s: per_s,
            gpu_allocated: report.as_ref().map(|r| r.total_allocated_bytes),
            gpu_reserved: report.as_ref().map(|r| r.total_reserved_bytes),
        };
        self.shared.status().memory = Some(memory);
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
        // A landing stays pinned while its request is the target: until
        // the presenter has put every stream's up and cleared the target,
        // or a newer request has taken its place. In a slot isn't enough:
        // with too few slots the presenter shows each stream's as it
        // comes, and a stream whose landing had left the cache would want
        // nothing and keep the slot another stream's needs. The target is
        // read again after the status, since a landing published there
        // came after its request's target was stored.
        let current = shared.target.load(Ordering::Acquire);
        st.unpinned.resize(n, 0);
        for (i, s) in streams.iter().enumerate() {
            if let Some(l) = s.landed
                && st.unpinned[i] != l.id
                && l.id != current
            {
                shared.cache.unpin(i, l.time);
                st.unpinned[i] = l.id;
            }
        }
        // While a stream's frame for the target isn't in a slot, every
        // stream wants its own alone: the frames a stream that landed
        // first would want after its own must leave the slot the others
        // need free (each stream's frame on screen stays up meanwhile).
        // With fewer than two slots a stream, the same holds for each
        // stream's frame at the playhead: one stream's next frame took the
        // slot another's needed when that one was decoded later, and
        // while the clock waited (paused, or for that frame) nothing
        // freed it.
        let active = slots.stats().active;
        let playhead = shared.cache.playhead();
        let waiting = if target != 0 {
            streams.iter().enumerate().any(|(i, s)| {
                !s.out
                    && !s
                        .landed
                        .is_some_and(|l| l.id == target && in_slot(i, l.time))
            })
        } else {
            active < 2 * n
                && (0..n).any(|i| {
                    self.at_playhead(i, playhead)
                        .is_some_and(|f| !in_slot(i, f.time))
                })
        };
        let quota = if waiting { 1 } else { (active / n).max(2) };
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
            self.at_playhead(i, playhead)
        }?;
        let mut out = vec![base.clone()];
        let mut ahead = base.clone();
        let mut more = |out: &mut Vec<Arc<VideoFrame>>| {
            // The frame after `ahead` in the stream: linked to it, or of
            // another run but starting where it ends (decoding went on
            // from the end of a run held). One past a gap waits for the
            // frames between: it is a run a seek left, and would hold the
            // slot they need (the frames past a seek back, at a loop).
            let next = cache
                .following(i, ahead.time)
                .filter(|(f, linked)| {
                    *linked || frames_between(&Place::of_frame(&ahead), &Place::of_frame(f)) == 0
                })
                .map(|(f, _)| f);
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

    /// Stream `i`'s frame at the playhead, as the cache holds it: the
    /// latest at or before it, else the first after it.
    fn at_playhead(&self, i: usize, playhead: Time) -> Option<Arc<VideoFrame>> {
        let cache = &self.shared.cache;
        cache
            .at_or_before(i, playhead)
            .or_else(|| cache.following(i, playhead).map(|(f, _)| f))
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
                        live: slots.stats().active,
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
        let uploaded = if f.dmabuf().is_some() || f.vulkan().is_some() {
            self.upload_on_device(st, f, placement, &gpu_frame)
        } else {
            st.uploader
                .upload_placed(&self.gpu, f.as_ref(), placement, &gpu_frame)
        };
        match uploaded {
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
                    live: slots.stats().active,
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

    /// Uploads `f`, left on its decoder's device, into `slot`: imported
    /// (DMA-BUFs) or copied out on the GPU (a Vulkan image), or
    /// downloaded when the GPU refuses it (whose reason the status keeps,
    /// the first time).
    fn upload_on_device(
        &self,
        st: &mut State,
        f: &Arc<VideoFrame>,
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let taken = if f.vulkan().is_some() {
            self.copy_out(st, f, placement, slot)
        } else {
            self.import(st, f, placement, slot)
        };
        let why = match taken {
            Err(RenderError::Import(why)) => why,
            other => return other,
        };
        self.shared.perf.add(Counter::ImportsRefused, 1);
        {
            let mut status = self.shared.status();
            if status.import_refused.is_none() {
                tracing::warn!("the GPU can't take the decoder's frames ({why}): downloading them");
                status.import_refused = Some(why);
            }
        }
        let start = Instant::now();
        let down = match f.to_system() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                return Err(RenderError::Invalid(format!(
                    "downloading a frame the GPU can't import: {e}"
                )));
            }
            None => return Err(RenderError::Invalid("a frame without planes".into())),
        };
        let mut stats = st
            .uploader
            .upload_placed(&self.gpu, &down, placement, slot)?;
        stats.cpu_ms = start.elapsed().as_secs_f32() * 1e3;
        Ok(stats)
    }

    /// `f`'s planes imported and copied into `slot`, and `f` held until
    /// the GPU has read them. `Import` when the device or its driver
    /// can't take them.
    fn import(
        &self,
        st: &mut State,
        f: &Arc<VideoFrame>,
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let layers = f.dmabuf().unwrap_or_default();
        let whole = SlotShape::of(f.layout(), f.size())?;
        let planes = dmabuf::plan(&whole, layers).map_err(RenderError::Import)?;
        let textures = st.importer.import(&self.gpu, &planes)?;
        let stats =
            st.uploader
                .upload_imported(&self.gpu, f.as_ref(), &textures, placement, slot)?;
        // The decoder writes the surface again once the frame is let go:
        // not before the GPU has read it. The textures wgpu keeps itself.
        let keep = f.clone();
        self.gpu.queue.on_submitted_work_done(move || drop(keep));
        self.shared.perf.add(Counter::FramesImported, 1);
        Ok(stats)
    }

    /// `f`'s planes copied out of its Vulkan image by the device's copier
    /// and into `slot`. The copy is waited for, so that `f` can go back to
    /// its decoder; the buffer is held until the GPU has read it. `Import`
    /// when the device isn't shared or the copier can't take the frame.
    fn copy_out(
        &self,
        st: &mut State,
        f: &Arc<VideoFrame>,
        placement: &Placement,
        slot: &GpuFrame,
    ) -> Result<UploadStats, RenderError> {
        let start = Instant::now();
        let shared = self.gpu.shared_video().map_err(RenderError::Import)?;
        let picture = f
            .vulkan()
            .ok_or_else(|| RenderError::Invalid("a frame without a Vulkan image".into()))?;
        let regions = upload::shown_regions(f.as_ref(), placement)?;
        // Diag: after the first 30, the copy or the GPU's part skipped.
        static COPIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let skipping = COPIES.fetch_add(1, Ordering::Relaxed) >= 30;
        let skipped = || UploadStats {
            cpu_ms: start.elapsed().as_secs_f32() * 1e3,
            bytes: 0,
            gpu_ms: None,
        };
        if skipping && diag::has("skip_copy") {
            return Ok(skipped());
        }
        diag::wait_gate("gate_copy");
        let t = Instant::now();
        let copied = shared.copier.copy(picture, &regions)?;
        shared.copier.wait(&copied)?;
        diag::note_call("copy", t);
        if skipping && diag::has("skip_gpu") {
            drop(copied);
            return Ok(skipped());
        }
        if diag::has("gate") {
            let t = Instant::now();
            diag::wait_next_pass(Duration::from_millis(20));
            diag::note_call("gate", t);
        }
        shared.copier.order_before(&self.gpu.queue, &copied);
        diag::wait_gate("gate_upload");
        let t = Instant::now();
        let mut stats = st.uploader.upload_from_buffer(
            &self.gpu,
            f.as_ref(),
            &copied.buffer,
            &copied.planes,
            placement,
            slot,
        )?;
        diag::note_call("upload", t);
        self.gpu.queue.on_submitted_work_done(move || drop(copied));
        stats.cpu_ms = start.elapsed().as_secs_f32() * 1e3;
        self.shared.perf.add(Counter::VulkanCopies, 1);
        Ok(stats)
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
