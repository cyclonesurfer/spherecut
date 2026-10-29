//! The bounded frame cache (M1 plan section 8.3, DECISIONS D34).
//!
//! Decoded frames, shared as `Arc<VideoFrame>` with the upload thread so
//! that no plane is copied, keyed by stream and clip time in a
//! `BTreeMap` (clip time orders a stream's frames as their pts do, and
//! compares with the playhead directly). Their bytes are counted from
//! the frames' own buffers ([`VideoFrame::bytes`]), padding included, and
//! never pass the budget but when what the cache must keep doesn't fit:
//! each stream's current frame and window (below). With frames of one
//! size, that is a budget too small to hold a stream's frame at the
//! playhead and the one after it, which the cache still holds so that
//! playback goes on (and says it is too small, below [`MIN_FRAMES`]
//! frames).
//!
//! **Runs.** A decoder numbers the frames it hands out by run (a seek
//! starts one) and place in the run, and two frames of one run whose
//! places follow on are adjacent in the stream (DECISIONS D29). The cache
//! links frames by those numbers, so the frame after or before a held
//! one is known without decoding: a step, forward or back, and a seek
//! into frames held already, are instant. A frame evicted takes its
//! links with it; the links between the frames left stand.
//!
//! **Policy.** The playhead splits each stream's frames:
//!
//! - the *current* frame, the latest at or before the playhead;
//! - frames *behind* it, which are history;
//! - the *window*: the frames after the playhead, nearest first, as far
//!   as `ahead` past the playhead and half the budget (shared between the
//!   streams), and always the nearest one;
//! - frames ahead past the window, left from before a seek back.
//!
//! The current frame and the window are never evicted. Over budget, the
//! frame farthest behind the playhead goes first, then the frame
//! farthest ahead past the window. A frame ahead becomes evictable only
//! once the playhead passes it (a seek, or playing), or nearer frames
//! fill the window ahead of it.
//!
//! **Back-pressure.** A frame ahead of the playhead goes in only if it
//! would be in the window: [`FrameCache::insert`] waits on a condition
//! variable until it would (the playhead moves on, the budget grows) or
//! its stop token says to stop, waking every [`WAIT_SLICE`] to look;
//! [`FrameCache::try_insert`] says [`Insert::Full`] instead. A frame at
//! or behind the playhead never waits: playback is waiting for it.
//!
//! **Never blocks the UI.** Every method that takes the lock calls
//! [`may_block`] first. The UI thread uses the `try_` methods, which
//! never wait for the lock and say so when it is taken.

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use spherecut_core::{Rational, StopToken, Time, may_block};
use spherecut_media::backend::VideoFrame;

/// A budget below this many frames of the source raises the "frame cache
/// too small" notice; playback goes on.
pub const MIN_FRAMES: u64 = 6;

/// How long a wait for room lasts before it looks at its stop token.
pub const WAIT_SLICE: Duration = Duration::from_millis(20);

/// Clip times no frame has, to bound ranges with.
const EARLIEST: Time = Time(Rational::from_integer(i64::MIN));
const LATEST: Time = Time(Rational::from_integer(i64::MAX));

/// What became of a frame offered to the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insert {
    /// It is held.
    Stored,
    /// A frame at its time was held already, from another run, and is
    /// kept; the new frame's run links to it now.
    Known,
    /// It is behind the playhead, and nothing farther behind made room
    /// for it, so it isn't held.
    Dropped,
    /// It is ahead of the playhead, past the window: from
    /// [`FrameCache::try_insert`], which doesn't wait.
    Full,
    /// Its stop token said to stop while it waited, or the cache was
    /// closed.
    Stopped,
}

/// The frame cache. Share it in an `Arc` between the decode workers, the
/// upload thread and the engine.
#[derive(Debug)]
pub struct FrameCache {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug)]
struct Entry {
    frame: Arc<VideoFrame>,
    bytes: u64,
    /// Every (run, place) the frame was handed out as.
    runs: Vec<(u64, u64)>,
}

#[derive(Debug)]
struct State {
    budget: u64,
    ahead: Time,
    playhead: Time,
    frames: BTreeMap<(usize, Time), Entry>,
    /// (stream, run, place) to the time of the frame held for it.
    links: BTreeMap<(usize, u64, u64), Time>,
    bytes: u64,
    /// The bytes of the last frame offered, per stream since the last
    /// clear: one frame of the source.
    frame_bytes: BTreeMap<usize, u64>,
    /// Bumped by every change.
    generation: u64,
    closed: bool,
    evicted: u64,
    dropped: u64,
    waits: u64,
}

/// What the cache holds, for the overlay and the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub budget: u64,
    pub bytes: u64,
    pub frames: usize,
    pub playhead: Time,
    /// Frames after the playhead, and their bytes.
    pub ahead: usize,
    pub ahead_bytes: u64,
    /// How far past the playhead the farthest window reaches.
    pub ahead_span: Time,
    /// Frames at or before the playhead (the current ones among them).
    pub behind: usize,
    /// The bytes of one frame of the source: the last frame offered of
    /// each stream, summed.
    pub frame_bytes: u64,
    /// The budget holds fewer than [`MIN_FRAMES`] frames of the source.
    pub too_small: bool,
    /// Frames evicted, frames not kept ([`Insert::Dropped`]), and inserts
    /// that had to wait for room, since the cache was made.
    pub evicted: u64,
    pub dropped: u64,
    pub waits: u64,
}

impl State {
    fn stream(&self, s: usize) -> std::collections::btree_map::Range<'_, (usize, Time), Entry> {
        self.frames
            .range((Bound::Included((s, EARLIEST)), Bound::Included((s, LATEST))))
    }

    /// The time of stream `s`'s current frame: the latest at or before
    /// the playhead.
    fn current(&self, s: usize) -> Option<Time> {
        self.frames
            .range((
                Bound::Included((s, EARLIEST)),
                Bound::Included((s, self.playhead)),
            ))
            .next_back()
            .map(|(&(_, t), _)| t)
    }

    /// Frames of stream `s` after the playhead, nearest first.
    fn ahead_of(&self, s: usize) -> impl Iterator<Item = (Time, &Entry)> {
        self.frames
            .range((
                Bound::Excluded((s, self.playhead)),
                Bound::Included((s, LATEST)),
            ))
            .map(|(&(_, t), e)| (t, e))
    }

    fn limit(&self) -> Time {
        self.playhead.checked_add(self.ahead).unwrap_or(LATEST)
    }

    /// The bytes each stream's window may hold: half the budget, shared.
    fn cap(&self) -> u64 {
        self.budget / 2 / self.frame_bytes.len().max(1) as u64
    }

    /// The time of the farthest frame in stream `s`'s window.
    fn window_end(&self, s: usize) -> Option<Time> {
        let (limit, cap) = (self.limit(), self.cap());
        let mut bytes = 0u64;
        let mut end = None;
        for (t, e) in self.ahead_of(s) {
            bytes = bytes.saturating_add(e.bytes);
            if end.is_some() && (t > limit || bytes > cap) {
                break;
            }
            end = Some(t);
        }
        end
    }

    /// Whether a frame of `bytes` at `t`, ahead of the playhead, would be
    /// in stream `s`'s window.
    fn admits(&self, s: usize, t: Time, bytes: u64) -> bool {
        let mut before = 0u64;
        let mut nearer = 0usize;
        for (u, e) in self.ahead_of(s) {
            if u >= t {
                break;
            }
            before = before.saturating_add(e.bytes);
            nearer += 1;
        }
        nearer == 0 || (t <= self.limit() && before.saturating_add(bytes) <= self.cap())
    }

    /// The frame to evict next: the farthest behind the playhead of any
    /// stream, else the farthest ahead past a window; `None` when every
    /// frame is a current one or in a window.
    fn victim(&self) -> Option<(usize, Time)> {
        let streams: Vec<usize> = self.frame_bytes.keys().copied().collect();
        let behind = streams
            .iter()
            .filter_map(|&s| {
                let current = self.current(s)?;
                let (&(_, t), _) = self.stream(s).next()?;
                (t < current).then_some((t, s))
            })
            .min();
        if let Some((t, s)) = behind {
            return Some((s, t));
        }
        streams
            .iter()
            .filter_map(|&s| {
                let (&(_, t), _) = self.stream(s).next_back()?;
                let end = self.window_end(s)?;
                (t > end).then_some((t, s))
            })
            .max()
            .map(|(t, s)| (s, t))
    }

    fn remove(&mut self, key: (usize, Time)) {
        if let Some(e) = self.frames.remove(&key) {
            self.bytes -= e.bytes;
            for (run, place) in e.runs {
                if self.links.get(&(key.0, run, place)) == Some(&key.1) {
                    self.links.remove(&(key.0, run, place));
                }
            }
        }
    }

    /// Evict until the budget holds what is left, or only current frames
    /// and windows are.
    fn shrink(&mut self) {
        while self.bytes > self.budget {
            let Some(key) = self.victim() else {
                break;
            };
            self.remove(key);
            self.evicted += 1;
        }
    }

    /// Put `frame` in, or `None` when it is ahead, past the window.
    fn offer(&mut self, s: usize, frame: &Arc<VideoFrame>) -> Option<Insert> {
        let (t, bytes) = (frame.time, frame.bytes() as u64);
        let place = (frame.run, frame.seq);
        self.frame_bytes.insert(s, bytes);
        let held = self.frames.contains_key(&(s, t));
        if !held && t > self.playhead && !self.admits(s, t, bytes) {
            return None;
        }
        // A link to another time is a decoder that numbered its runs
        // afresh: the old one goes, so that no false link is made.
        if let Some(&old) = self.links.get(&(s, place.0, place.1))
            && old != t
        {
            self.links.remove(&(s, place.0, place.1));
            if let Some(e) = self.frames.get_mut(&(s, old)) {
                e.runs.retain(|&p| p != place);
            }
        }
        let outcome = match self.frames.get_mut(&(s, t)) {
            Some(e) => {
                // Two frames of one run at one time (a broken mux's
                // timestamps tie): the later shows, so it replaces the
                // earlier, and both places link to it.
                let tie = e
                    .runs
                    .iter()
                    .any(|&(run, p)| run == place.0 && p != place.1);
                if !e.runs.contains(&place) {
                    e.runs.push(place);
                }
                if tie {
                    self.bytes = self.bytes - e.bytes + bytes;
                    e.frame = frame.clone();
                    e.bytes = bytes;
                    Insert::Stored
                } else {
                    Insert::Known
                }
            }
            None => {
                self.frames.insert(
                    (s, t),
                    Entry {
                        frame: frame.clone(),
                        bytes,
                        runs: vec![place],
                    },
                );
                self.bytes += bytes;
                Insert::Stored
            }
        };
        self.links.insert((s, place.0, place.1), t);
        self.shrink();
        self.generation += 1;
        if self.frames.contains_key(&(s, t)) {
            Some(outcome)
        } else {
            self.dropped += 1;
            Some(Insert::Dropped)
        }
    }

    fn get(&self, s: usize, t: Time) -> Option<Arc<VideoFrame>> {
        self.frames.get(&(s, t)).map(|e| e.frame.clone())
    }

    /// The frame linked after (`step` 1) or before (`step` −1) the one at
    /// `t`: through any of its runs, the first frame in decode order whose
    /// time is past (before) `t`, passing over frames tied with it.
    fn linked(&self, s: usize, t: Time, step: i8) -> Option<Arc<VideoFrame>> {
        let e = self.frames.get(&(s, t))?;
        e.runs.iter().find_map(|&(run, place)| {
            let mut p = place;
            loop {
                p = if step > 0 {
                    p.checked_add(1)?
                } else {
                    p.checked_sub(1)?
                };
                let &u = self.links.get(&(s, run, p))?;
                if (step > 0 && u > t) || (step < 0 && u < t) {
                    return self.get(s, u);
                }
            }
        })
    }

    fn stats(&self) -> CacheStats {
        let mut ahead = 0;
        let mut ahead_bytes = 0;
        let mut ahead_span = Time::ZERO;
        for &s in self.frame_bytes.keys() {
            for (_, e) in self.ahead_of(s) {
                ahead += 1;
                ahead_bytes += e.bytes;
            }
            if let Some(end) = self.window_end(s) {
                ahead_span = ahead_span.max(end.checked_sub(self.playhead).unwrap_or(LATEST));
            }
        }
        let frame_bytes = self.frame_bytes.values().sum::<u64>();
        CacheStats {
            budget: self.budget,
            bytes: self.bytes,
            frames: self.frames.len(),
            playhead: self.playhead,
            ahead,
            ahead_bytes,
            ahead_span,
            behind: self.frames.len() - ahead,
            frame_bytes,
            too_small: frame_bytes > 0 && self.budget < frame_bytes.saturating_mul(MIN_FRAMES),
            evicted: self.evicted,
            dropped: self.dropped,
            waits: self.waits,
        }
    }
}

impl FrameCache {
    /// An empty cache holding at most `budget` bytes, with frames kept
    /// as far as `ahead` past the playhead, which is at 0.
    pub fn new(budget: u64, ahead: Time) -> Self {
        FrameCache {
            state: Mutex::new(State {
                budget,
                ahead,
                playhead: Time::ZERO,
                frames: BTreeMap::new(),
                links: BTreeMap::new(),
                bytes: 0,
                frame_bytes: BTreeMap::new(),
                generation: 0,
                closed: false,
                evicted: 0,
                dropped: 0,
                waits: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        may_block("taking the frame cache's lock");
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold the lock, as a busy decode or upload thread does, for the
    /// tests of what the UI thread does meanwhile.
    #[cfg(test)]
    pub(crate) fn hold_lock(&self) -> impl Sized + '_ {
        self.lock()
    }

    fn try_lock(&self) -> Option<MutexGuard<'_, State>> {
        match self.state.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Change the state under the lock `g`, and wake everyone waiting.
    fn changed<T>(&self, mut g: MutexGuard<'_, State>, f: impl FnOnce(&mut State) -> T) -> T {
        let r = f(&mut g);
        g.generation += 1;
        drop(g);
        self.changed.notify_all();
        r
    }

    /// Put `frame`, of stream `stream`, in. A frame ahead of the playhead
    /// past the window waits until it would be in it, or `stop` says to
    /// stop, or the cache is closed ([`Insert::Stopped`]). Blocks: never
    /// on the UI thread.
    pub fn insert(&self, stream: usize, frame: Arc<VideoFrame>, stop: &StopToken) -> Insert {
        may_block("waiting for room in the frame cache");
        let mut g = self.lock();
        let mut waited = false;
        loop {
            if g.closed || stop.is_stopped() {
                return Insert::Stopped;
            }
            if let Some(r) = g.offer(stream, &frame) {
                drop(g);
                self.changed.notify_all();
                return r;
            }
            if !waited {
                g.waits += 1;
                waited = true;
            }
            g = self
                .changed
                .wait_timeout(g, WAIT_SLICE)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// [`insert`](Self::insert) without waiting for room:
    /// [`Insert::Full`] instead. Still takes the lock.
    pub fn try_insert(&self, stream: usize, frame: Arc<VideoFrame>) -> Insert {
        let mut g = self.lock();
        if g.closed {
            return Insert::Stopped;
        }
        match g.offer(stream, &frame) {
            Some(r) => {
                drop(g);
                self.changed.notify_all();
                r
            }
            None => Insert::Full,
        }
    }

    /// [`try_insert`](Self::try_insert), unless `stop` says to stop
    /// ([`Insert::Stopped`]), which it reads under the lock: a worker
    /// whose file was closed meanwhile never puts a frame of it in the
    /// cache the next file uses.
    pub fn try_insert_until(
        &self,
        stream: usize,
        frame: Arc<VideoFrame>,
        stop: &StopToken,
    ) -> Insert {
        let mut g = self.lock();
        if g.closed || stop.is_stopped() {
            return Insert::Stopped;
        }
        match g.offer(stream, &frame) {
            Some(r) => {
                drop(g);
                self.changed.notify_all();
                r
            }
            None => Insert::Full,
        }
    }

    /// Move the playhead to clip time `at`, which wakes a decode worker
    /// waiting for room.
    pub fn set_playhead(&self, at: Time) {
        let g = self.lock();
        self.changed(g, |s| s.playhead = at);
    }

    /// [`set_playhead`](Self::set_playhead) if the lock is free; false
    /// when it isn't and nothing changed, to try again next frame. Never
    /// waits: for the UI thread.
    pub fn try_set_playhead(&self, at: Time) -> bool {
        match self.try_lock() {
            Some(g) => {
                self.changed(g, |s| s.playhead = at);
                true
            }
            None => false,
        }
    }

    pub fn playhead(&self) -> Time {
        self.lock().playhead
    }

    /// Hold at most `bytes` from now on, evicting down to it.
    pub fn set_budget(&self, bytes: u64) {
        let g = self.lock();
        self.changed(g, |s| {
            s.budget = bytes;
            s.shrink();
        });
    }

    /// Keep frames as far as `ahead` past the playhead.
    pub fn set_ahead(&self, ahead: Time) {
        let g = self.lock();
        self.changed(g, |s| s.ahead = ahead);
    }

    /// Drop every frame and link: a new file.
    pub fn clear(&self) {
        let g = self.lock();
        self.changed(g, |s| {
            s.frames.clear();
            s.links.clear();
            s.frame_bytes.clear();
            s.bytes = 0;
        });
    }

    /// Drop stream `stream`'s links, keeping its frames: for a new
    /// decoder on the stream, whose runs are numbered afresh.
    pub fn forget_runs(&self, stream: usize) {
        let g = self.lock();
        self.changed(g, |s| {
            s.links.retain(|&(st, _, _), _| st != stream);
            for ((st, _), e) in s.frames.iter_mut() {
                if *st == stream {
                    e.runs.clear();
                }
            }
        });
    }

    /// Stop every wait, now and from now on: at shutdown.
    pub fn close(&self) {
        let g = self.lock();
        self.changed(g, |s| s.closed = true);
    }

    /// Wake everyone waiting, to look at their stop tokens: after a seek
    /// has stopped the ones before it.
    pub fn wake(&self) {
        let g = self.lock();
        self.changed(g, |_| ());
    }

    /// Wake everyone waiting without taking the lock, for the UI thread
    /// after it has stopped their tokens (a seek, shutdown). A waiter
    /// that was about to wait when this ran misses it, and looks at its
    /// token within [`WAIT_SLICE`] anyway. Never waits.
    pub fn notify(&self) {
        self.changed.notify_all();
    }

    /// The frame of `stream` at exactly `time`.
    pub fn get(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        self.lock().get(stream, time)
    }

    /// The frame shown at `at`, when the cache can tell: the latest at or
    /// before `at`, if the frame linked after it is later than `at`, so
    /// that no frame lies between. A frame whose successor isn't held
    /// can't be told from one followed by frames not decoded yet.
    pub fn frame_at(&self, stream: usize, at: Time) -> Option<Arc<VideoFrame>> {
        let g = self.lock();
        let (&(_, t), e) = g
            .frames
            .range((
                Bound::Included((stream, EARLIEST)),
                Bound::Included((stream, at)),
            ))
            .next_back()?;
        let next = g.linked(stream, t, 1)?;
        (next.time > at).then(|| e.frame.clone())
    }

    /// The latest frame of `stream` at or before `at`, linked or not.
    pub fn at_or_before(&self, stream: usize, at: Time) -> Option<Arc<VideoFrame>> {
        let g = self.lock();
        g.frames
            .range((
                Bound::Included((stream, EARLIEST)),
                Bound::Included((stream, at)),
            ))
            .next_back()
            .map(|(_, e)| e.frame.clone())
    }

    /// The first frame of `stream` after `time` held, linked or not, and
    /// whether it is linked to the frame at `time`, which says that none
    /// lies between.
    pub fn following(&self, stream: usize, time: Time) -> Option<(Arc<VideoFrame>, bool)> {
        let g = self.lock();
        let (_, e) = g
            .frames
            .range((
                Bound::Excluded((stream, time)),
                Bound::Included((stream, LATEST)),
            ))
            .next()?;
        let linked = g
            .linked(stream, time, 1)
            .is_some_and(|n| n.time == e.frame.time);
        Some((e.frame.clone(), linked))
    }

    /// The frame after the one of `stream` at `time`, by its run links:
    /// adjacent in the stream.
    pub fn next_after(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        self.lock().linked(stream, time, 1)
    }

    /// The frame before the one of `stream` at `time`, by its run links.
    pub fn prev_before(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        self.lock().linked(stream, time, -1)
    }

    /// The last frame reached from the one of `stream` at `time` through
    /// run links: where a decoder goes on from after a seek into frames
    /// held already. `None` when no frame is held at `time`.
    pub fn run_end(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        let g = self.lock();
        let mut at = g.get(stream, time)?;
        while let Some(next) = g.linked(stream, at.time, 1) {
            at = next;
        }
        Some(at)
    }

    /// Bumped by every change: compare it to see whether anything did.
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Wait until the generation isn't `seen`, or `timeout` has passed,
    /// and return it. Blocks: for the upload thread, waiting for a frame
    /// to upload.
    pub fn wait_for_change(&self, seen: u64, timeout: Duration) -> u64 {
        may_block("waiting for the frame cache to change");
        let deadline = Instant::now() + timeout;
        let mut g = self.lock();
        while g.generation == seen && !g.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self
                .changed
                .wait_timeout(g, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        g.generation
    }

    pub fn stats(&self) -> CacheStats {
        self.lock().stats()
    }

    /// [`stats`](Self::stats) if the lock is free, `None` when it isn't,
    /// to show the last ones again. Never waits: for the UI thread.
    pub fn try_stats(&self) -> Option<CacheStats> {
        self.try_lock().map(|g| g.stats())
    }

    /// Every frame held, with its stream, in stream and time order: for
    /// tests.
    pub fn held(&self) -> Vec<(usize, Arc<VideoFrame>)> {
        self.lock()
            .frames
            .iter()
            .map(|(&(s, _), e)| (s, e.frame.clone()))
            .collect()
    }
}
