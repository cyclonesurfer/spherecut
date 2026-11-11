//! The bounded frame cache (M1 plan section 8.3, DECISIONS D34).
//!
//! Decoded frames, shared as `Arc<VideoFrame>` with the upload thread so
//! that no plane is copied, keyed by stream and clip time in a
//! `BTreeMap` (clip time orders a stream's frames as their pts do, and
//! compares with the playhead directly). Their bytes are counted from
//! the frames' own buffers ([`VideoFrame::bytes`]), padding included, and
//! never pass the budget but when what the cache must keep doesn't fit:
//! each stream's current frame and window, and its pinned frame (below).
//! With frames of one size and nothing pinned, that is a budget too
//! small to hold a stream's frame at the playhead and the one after it,
//! which the cache still holds so that playback goes on (and says it is
//! too small, below [`MIN_FRAMES`] frames); a pinned frame can add one
//! more a stream, for as long as the presenter waits for it. Frames are
//! evicted as frames go in, a pin is let go or the budget changes;
//! moving the playhead evicts nothing, so a cache over its budget (by a
//! pinned frame, say) may hold, until the next of those, frames the move
//! left to the rules.
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
//! **Pins.** Each stream may pin one frame ([`FrameCache::pin`]): the
//! one a request landed on, which the presenter waits for until every
//! stream's is on screen. A pinned frame is never evicted and goes in past the
//! window too, until it is unpinned, the stream pins another, or the
//! cache is cleared. Without it, a keyframe seek back would lose the
//! keyframe it landed on to the frames decoded on from it (all the
//! history is ahead after a seek back, so the keyframe is the frame
//! farthest behind), and a step on one stream to the playhead another
//! stream's worker moved.
//!
//! **Surfaces** (slice 5b). A frame left on its decoder's device
//! ([`Transfer::DrmPrime`], exported from VA-API, or
//! [`Transfer::Vulkan`], a Vulkan Video image) holds one of the surfaces of its decoder's pool, which on
//! VA-API has only so many and on Vulkan takes video memory the budget
//! doesn't count: the cache holds at most
//! [`set_surface_cap`](FrameCache::set_surface_cap) such frames a
//! stream. They are counted beside the bytes (they hold none of system
//! memory): a stream's window takes at most the cap less one of them,
//! leaving room for its current frame, and past the cap the stream's
//! frame farthest behind goes, then its frame farthest past its window,
//! as over the budget. A pinned frame may add one more. Without a cap
//! (the default) they are only bytes, of which they have none.
//!
//! **Back-pressure.** A frame ahead of the playhead goes in only if it
//! would be in the window: [`FrameCache::insert`] waits on a condition
//! variable until it would (the playhead moves on, the budget grows) or
//! its stop token says to stop, waking every [`WAIT_SLICE`] to look;
//! [`FrameCache::try_insert`] says [`Insert::Full`] instead. A frame at
//! or behind the playhead never waits: playback is waiting for it.
//!
//! **Locked pairs** (M3 plan section 9). With a group set
//! ([`FrameCache::set_group`]), the rules above run for its members from
//! the group base instead of the playhead: the newest time at or before
//! the playhead at which every member holds a frame, so that each
//! member's frame there is its current one and the faster lens decodes at
//! most its window past it. A seek or a step holds the base at the
//! playhead until every member has landed or failed it. When every
//! member waits for room decoding ahead and no time past the base has a
//! frame of each, the backstop holds the base at the trailing member's
//! newest frame, which each member seeks to ([`Insert::Resync`]).
//!
//! **Frames alive.** The budget counts the frames the cache holds. Others
//! are alive for a while beside them: the frame a worker decoded and
//! waits to insert, and frames evicted while the upload thread or a
//! worker still holds them, until its pass ends or it moves on. So the
//! decoded frames alive pass the budget by about a frame a stream (5.7K
//! frames through a 256 MiB cache: 261 MiB alive at worst, 237 MiB of it
//! held), about 88 MB with the X5's two 8K streams. Auto leaves three
//! quarters of the memory available for that and the rest.
//!
//! **Never blocks the UI.** Every method that takes the lock calls
//! [`may_block`] first, and tracks its caller, so that a report names the
//! line that called the cache. The UI thread uses the `try_` methods,
//! which never wait for the lock and say so when it is taken.

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use spherecut_core::{Rational, StopToken, Time, may_block};
use spherecut_media::backend::{Transfer, VideoFrame};

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
    /// It isn't held: the eviction its coming in set off took it, as the
    /// rules say. It is behind the playhead, and nothing farther behind
    /// made room for it; or it was held already, in a cache a playhead
    /// move left over its budget or its surface cap, and the move left it
    /// the next to go (ahead past the window, or behind).
    Dropped,
    /// It is ahead of the playhead, past the window: from
    /// [`FrameCache::try_insert`], which doesn't wait.
    Full,
    /// Its stop token said to stop while it waited, or the cache was
    /// closed.
    Stopped,
    /// It isn't held: every member of a locked pair's group waited for
    /// room with no time past the group base at which each holds a frame,
    /// so the cache holds the base at `at`, the trailing member's newest
    /// frame, and the worker seeks exactly there and puts that frame in
    /// instead (the backstop, from [`FrameCache::insert_ahead`] only).
    Resync { at: Time },
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
    /// It holds one of its decoder's surfaces.
    surface: bool,
    /// Every (run, place) the frame was handed out as.
    runs: Vec<(u64, u64)>,
}

#[derive(Debug)]
struct State {
    budget: u64,
    /// The frames holding a surface each stream may hold.
    surface_cap: Option<usize>,
    /// The frames holding a surface each stream holds, kept as frames
    /// come and go: counting them under the lock at each offer slows the
    /// presenter's tries for it.
    surfaces: BTreeMap<usize, usize>,
    ahead: Time,
    playhead: Time,
    frames: BTreeMap<(usize, Time), Entry>,
    /// (stream, run, place) to the time of the frame held for it.
    links: BTreeMap<(usize, u64, u64), Time>,
    bytes: u64,
    /// The bytes of the last frame offered, per stream since the last
    /// clear: one frame of the source.
    frame_bytes: BTreeMap<usize, u64>,
    /// The time of each stream's pinned frame.
    pins: BTreeMap<usize, Time>,
    /// Bumped by every change.
    generation: u64,
    closed: bool,
    evicted: u64,
    dropped: u64,
    waits: u64,
    /// A locked pair's group, while one is set.
    group: Option<Group>,
    /// The streams whose inserts wait for room now, and how each waits.
    waiting: BTreeMap<usize, Waiter>,
    /// The newest request each stream has landed or failed.
    landed: BTreeMap<usize, u64>,
    resyncs: u64,
}

/// A locked pair's group (M3 plan section 9): its members' rules run
/// from its base.
#[derive(Debug)]
struct Group {
    members: [usize; 2],
    /// The group base Tg.
    base: Time,
    hold: Option<Hold>,
    /// The newest request a hold was made for: an older one's worker,
    /// stopped on its way, makes none.
    request: u64,
}

/// An insert waiting for room.
#[derive(Debug)]
struct Waiter {
    /// The time of the frame it waits with.
    time: Time,
    /// Whether it decodes ahead ([`FrameCache::insert_ahead`]), which the
    /// backstop counts.
    ahead: bool,
    /// Its stop token: one already stopped is on its way out, to a newer
    /// request, and the backstop doesn't count it.
    stop: StopToken,
    /// Which wait it is, the count of waits when it began: a stopped
    /// waiter of an earlier open that wakes late takes out its own entry,
    /// not the one a newer worker of its stream put in since.
    wait: u64,
}

/// What holds a group's base where it is for now, each member's flag
/// set until it has done its part.
#[derive(Debug, Clone, Copy)]
enum Hold {
    /// Request `id`, a seek or a step, is on its way: the base is the
    /// playhead until every member has landed or failed it.
    Request { id: u64, pending: [bool; 2] },
    /// The backstop sought the members to `at`: the base stays there
    /// until each has been told ([`Insert::Resync`]) and put its frame
    /// there in, or failed.
    Resync {
        at: Time,
        untold: [bool; 2],
        pending: [bool; 2],
    },
}

impl Hold {
    fn pending(&mut self) -> &mut [bool; 2] {
        match self {
            Hold::Request { pending, .. } | Hold::Resync { pending, .. } => pending,
        }
    }
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
    /// The frames holding a decoder's surface, of every stream.
    pub surfaces: usize,
    /// The group base, while a locked pair's group is set
    /// ([`FrameCache::group_base`]).
    pub base: Option<Time>,
    /// Inserts waiting for room now.
    pub waiting: usize,
    /// Times the backstop sought a group's members to the trailing
    /// member's newest frame ([`Insert::Resync`]), since the cache was
    /// made.
    pub resyncs: u64,
}

impl State {
    fn stream(&self, s: usize) -> std::collections::btree_map::Range<'_, (usize, Time), Entry> {
        self.frames
            .range((Bound::Included((s, EARLIEST)), Bound::Included((s, LATEST))))
    }

    /// Where stream `s`'s rules run from: the group base for a member of
    /// a locked pair's group, else the playhead.
    fn head(&self, s: usize) -> Time {
        match &self.group {
            Some(g) if g.members.contains(&s) => g.base,
            _ => self.playhead,
        }
    }

    /// The time of stream `s`'s current frame: the latest at or before
    /// its head.
    fn current(&self, s: usize) -> Option<Time> {
        self.frames
            .range((
                Bound::Included((s, EARLIEST)),
                Bound::Included((s, self.head(s))),
            ))
            .next_back()
            .map(|(&(_, t), _)| t)
    }

    /// Frames of stream `s` after its head, nearest first.
    fn ahead_of(&self, s: usize) -> impl Iterator<Item = (Time, &Entry)> {
        self.frames
            .range((
                Bound::Excluded((s, self.head(s))),
                Bound::Included((s, LATEST)),
            ))
            .map(|(&(_, t), e)| (t, e))
    }

    fn pinned(&self, s: usize, t: Time) -> bool {
        self.pins.get(&s) == Some(&t)
    }

    /// How far stream `s`'s window may reach.
    fn limit(&self, s: usize) -> Time {
        self.head(s).checked_add(self.ahead).unwrap_or(LATEST)
    }

    /// The largest time at or before `at` at which each of `members`
    /// holds a frame.
    fn common_at_or_before(&self, members: [usize; 2], at: Time) -> Option<Time> {
        let times = |s: usize| {
            self.frames
                .range((Bound::Included((s, EARLIEST)), Bound::Included((s, at))))
                .rev()
                .map(|(&(_, t), _)| t)
        };
        let (mut xs, mut ys) = (times(members[0]), times(members[1]));
        let (mut x, mut y) = (xs.next()?, ys.next()?);
        loop {
            match x.cmp(&y) {
                std::cmp::Ordering::Equal => return Some(x),
                std::cmp::Ordering::Greater => x = xs.next()?,
                std::cmp::Ordering::Less => y = ys.next()?,
            }
        }
    }

    /// Whether some time after `after` has a frame of each of `members`.
    fn common_after(&self, members: [usize; 2], after: Time) -> bool {
        let times = |s: usize| {
            self.frames
                .range((Bound::Excluded((s, after)), Bound::Included((s, LATEST))))
                .map(|(&(_, t), _)| t)
        };
        let (mut xs, mut ys) = (times(members[0]), times(members[1]));
        let (Some(mut x), Some(mut y)) = (xs.next(), ys.next()) else {
            return false;
        };
        loop {
            let next = match x.cmp(&y) {
                std::cmp::Ordering::Equal => return true,
                std::cmp::Ordering::Less => xs.next().map(|t| x = t),
                std::cmp::Ordering::Greater => ys.next().map(|t| y = t),
            };
            if next.is_none() {
                return false;
            }
        }
    }

    /// Work the group base out again: the backstop's time while its seek
    /// is on its way, the playhead while a request is, and otherwise the
    /// largest time at or before the playhead at which every member holds
    /// a frame, or the playhead when there is none.
    fn rebase(&mut self) {
        let Some(g) = &self.group else {
            return;
        };
        let base = match g.hold {
            Some(Hold::Resync { at, .. }) => at,
            Some(Hold::Request { .. }) => self.playhead,
            None => self
                .common_at_or_before(g.members, self.playhead)
                .unwrap_or(self.playhead),
        };
        if let Some(g) = &mut self.group {
            g.base = base;
        }
    }

    /// Member `s` of the group has done its part of the hold: landed or
    /// failed request `request`, or (`None`) put its frame at the
    /// backstop's time in or failed to. The hold ends once every member
    /// has; the caller works the base out again.
    fn done(&mut self, s: usize, request: Option<u64>) {
        let Some(g) = &mut self.group else {
            return;
        };
        let Some(i) = g.members.iter().position(|&m| m == s) else {
            return;
        };
        let Some(hold) = &mut g.hold else {
            return;
        };
        let ours = match (*hold, request) {
            (Hold::Request { id, .. }, Some(r)) => id == r,
            (Hold::Resync { .. }, None) => true,
            _ => false,
        };
        if !ours {
            return;
        }
        hold.pending()[i] = false;
        if hold.pending().iter().all(|&p| !p) {
            g.hold = None;
        }
    }

    /// Lock `members` together, or no longer with `None`; the same
    /// members again keep the group as it is.
    fn set_group(&mut self, members: Option<[usize; 2]>) {
        if self.group.as_ref().map(|g| g.members) == members {
            return;
        }
        self.group = members.map(|members| Group {
            members,
            base: self.playhead,
            hold: None,
            request: 0,
        });
        self.rebase();
    }

    /// The backstop's time, while it holds the group base.
    fn resync_at(&self) -> Option<Time> {
        match self.group.as_ref()?.hold {
            Some(Hold::Resync { at, .. }) => Some(at),
            _ => None,
        }
    }

    /// The backstop's time for member `s`, the first time it asks after
    /// the backstop held the base: it seeks there.
    fn tell_resync(&mut self, s: usize) -> Option<Time> {
        let g = self.group.as_mut()?;
        let i = g.members.iter().position(|&m| m == s)?;
        match &mut g.hold {
            Some(Hold::Resync { at, untold, .. }) if untold[i] => {
                untold[i] = false;
                Some(*at)
            }
            _ => None,
        }
    }

    /// The backstop (M3 plan section 9): when every member of the group
    /// waits for room decoding ahead with its token unstopped, nothing
    /// holds the base, and no time past it has a frame of each member,
    /// nothing would ever move the base on. Hold it at the trailing
    /// member's newest frame, the earliest of the frames they wait with,
    /// for each to seek to. Whether it did.
    fn backstop(&mut self) -> bool {
        let Some(g) = &self.group else {
            return false;
        };
        if g.hold.is_some() {
            return false;
        }
        let (members, base) = (g.members, g.base);
        let mut at: Option<Time> = None;
        for m in members {
            match self.waiting.get(&m) {
                Some(w) if w.ahead && !w.stop.is_stopped() => {
                    at = Some(at.map_or(w.time, |a| a.min(w.time)));
                }
                _ => return false,
            }
        }
        let Some(at) = at else {
            return false;
        };
        if self.common_after(members, base) {
            return false;
        }
        tracing::warn!("a locked pair has no common frame ahead: both lenses seek to {at:?}");
        if let Some(g) = &mut self.group {
            g.hold = Some(Hold::Resync {
                at,
                untold: [true; 2],
                pending: [true; 2],
            });
        }
        self.resyncs += 1;
        self.rebase();
        self.generation += 1;
        true
    }

    /// The bytes each stream's window may hold: half the budget, shared.
    fn cap(&self) -> u64 {
        self.budget / 2 / self.frame_bytes.len().max(1) as u64
    }

    /// The frames holding a surface each stream's window may hold: the
    /// cap less one, for the current frame.
    fn window_surfaces(&self) -> usize {
        self.surface_cap.map_or(usize::MAX, |c| c.saturating_sub(1))
    }

    /// The time of the farthest frame in stream `s`'s window.
    fn window_end(&self, s: usize) -> Option<Time> {
        let (limit, cap) = (self.limit(s), self.cap());
        let mut bytes = 0u64;
        let mut surfaces = 0usize;
        let mut end = None;
        for (t, e) in self.ahead_of(s) {
            bytes = bytes.saturating_add(e.bytes);
            surfaces += usize::from(e.surface);
            if end.is_some() && (t > limit || bytes > cap || surfaces > self.window_surfaces()) {
                break;
            }
            end = Some(t);
        }
        end
    }

    /// Whether a frame of `bytes` at `t`, ahead of stream `s`'s head,
    /// would be in its window; `surface` when it holds one.
    fn admits(&self, s: usize, t: Time, bytes: u64, surface: bool) -> bool {
        let mut before = 0u64;
        let mut nearer = 0usize;
        let mut surfaces = usize::from(surface);
        for (u, e) in self.ahead_of(s) {
            if u >= t {
                break;
            }
            before = before.saturating_add(e.bytes);
            nearer += 1;
            surfaces += usize::from(e.surface);
        }
        nearer == 0
            || (t <= self.limit(s)
                && before.saturating_add(bytes) <= self.cap()
                && surfaces <= self.window_surfaces())
    }

    /// The frame to evict next for the surface cap: of the first stream
    /// over it that has one to evict, the one [`victim`](Self::victim)'s
    /// order picks among its frames with a surface.
    fn surface_victim(&self) -> Option<(usize, Time)> {
        let cap = self.surface_cap?;
        self.surfaces
            .iter()
            .filter(|&(_, &n)| n > cap)
            .find_map(|(&s, _)| self.victim_among(&[s], |e| e.surface))
    }

    /// The frame to evict next: the farthest behind the playhead of any
    /// stream, else the farthest ahead past a window, passing over pinned
    /// frames; `None` when every frame is a current one, in a window or
    /// pinned.
    fn victim(&self) -> Option<(usize, Time)> {
        let streams: Vec<usize> = self.frame_bytes.keys().copied().collect();
        self.victim_among(&streams, |_| true)
    }

    /// [`victim`](Self::victim) among `streams`' frames that `take` takes.
    fn victim_among(
        &self,
        streams: &[usize],
        take: impl Fn(&Entry) -> bool,
    ) -> Option<(usize, Time)> {
        let times = |s: usize| {
            self.stream(s)
                .filter(|(_, e)| take(e))
                .map(|(&(_, t), _)| t)
        };
        let behind = streams
            .iter()
            .filter_map(|&s| {
                let current = self.current(s)?;
                let t = times(s)
                    .take_while(|&t| t < current)
                    .find(|&t| !self.pinned(s, t))?;
                Some((t, s))
            })
            .min();
        if let Some((t, s)) = behind {
            return Some((s, t));
        }
        streams
            .iter()
            .filter_map(|&s| {
                let end = self.window_end(s)?;
                let t = times(s)
                    .rev()
                    .take_while(|&t| t > end)
                    .find(|&t| !self.pinned(s, t))?;
                Some((t, s))
            })
            .max()
            .map(|(t, s)| (s, t))
    }

    /// Count a frame holding a surface in or out of stream `s`.
    fn count_surface(&mut self, s: usize, surface: bool, add: bool) {
        if !surface {
            return;
        }
        let n = self.surfaces.entry(s).or_default();
        if add {
            *n += 1;
        } else {
            *n = n.saturating_sub(1);
        }
    }

    fn remove(&mut self, key: (usize, Time)) {
        if let Some(e) = self.frames.remove(&key) {
            self.count_surface(key.0, e.surface, false);
            self.bytes -= e.bytes;
            for (run, place) in e.runs {
                if self.links.get(&(key.0, run, place)) == Some(&key.1) {
                    self.links.remove(&(key.0, run, place));
                }
            }
        }
    }

    /// Evict until the budget holds what is left and no stream holds
    /// more frames with a surface than the cap, or only current frames,
    /// windows and pinned frames are.
    fn shrink(&mut self) {
        loop {
            let key = if self.bytes > self.budget {
                self.victim()
            } else {
                self.surface_victim()
            };
            let Some(key) = key else {
                break;
            };
            self.remove(key);
            self.evicted += 1;
        }
    }

    /// Put `frame` in, or `None` when it is ahead, past the window (and
    /// not pinned).
    fn offer(&mut self, s: usize, frame: &Arc<VideoFrame>) -> Option<Insert> {
        let (t, bytes) = (frame.time, frame.bytes() as u64);
        let surface = matches!(frame.transfer, Transfer::DrmPrime | Transfer::Vulkan);
        let place = (frame.run, frame.seq);
        self.frame_bytes.insert(s, bytes);
        let held = self.frames.contains_key(&(s, t));
        if !held && t > self.head(s) && !self.pinned(s, t) && !self.admits(s, t, bytes, surface) {
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
                    let was = std::mem::replace(&mut e.surface, surface);
                    let n = self.surfaces.entry(s).or_default();
                    *n = (*n + usize::from(surface)).saturating_sub(usize::from(was));
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
                        surface,
                        runs: vec![place],
                    },
                );
                self.bytes += bytes;
                self.count_surface(s, surface, true);
                Insert::Stored
            }
        };
        self.links.insert((s, place.0, place.1), t);
        // The base before the eviction, which keeps each member's frame
        // at it as its current one.
        if self.resync_at() == Some(t) {
            self.done(s, None);
        }
        self.rebase();
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
                ahead_span = ahead_span.max(end.checked_sub(self.head(s)).unwrap_or(LATEST));
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
            surfaces: self.surfaces.values().sum(),
            base: self.group.as_ref().map(|g| g.base),
            waiting: self.waiting.len(),
            resyncs: self.resyncs,
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
                surface_cap: None,
                surfaces: BTreeMap::new(),
                ahead,
                playhead: Time::ZERO,
                frames: BTreeMap::new(),
                links: BTreeMap::new(),
                bytes: 0,
                frame_bytes: BTreeMap::new(),
                pins: BTreeMap::new(),
                generation: 0,
                closed: false,
                evicted: 0,
                dropped: 0,
                waits: 0,
                group: None,
                waiting: BTreeMap::new(),
                landed: BTreeMap::new(),
                resyncs: 0,
            }),
            changed: Condvar::new(),
        }
    }

    #[track_caller]
    fn lock(&self) -> MutexGuard<'_, State> {
        may_block("taking the frame cache's lock");
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold the lock, as a busy decode or upload thread does, for the
    /// tests of what the UI thread does meanwhile.
    #[cfg(test)]
    #[track_caller]
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
    #[track_caller]
    pub fn insert(&self, stream: usize, frame: Arc<VideoFrame>, stop: &StopToken) -> Insert {
        self.insert_with(stream, frame, stop, false)
    }

    /// [`insert`](Self::insert) for a frame decoded ahead, between
    /// requests: a member of a locked pair's group waiting here counts
    /// for the backstop, and when the backstop holds the group base it
    /// gets [`Insert::Resync`] instead, with the time to seek to.
    #[track_caller]
    pub fn insert_ahead(&self, stream: usize, frame: Arc<VideoFrame>, stop: &StopToken) -> Insert {
        self.insert_with(stream, frame, stop, true)
    }

    #[track_caller]
    fn insert_with(
        &self,
        stream: usize,
        frame: Arc<VideoFrame>,
        stop: &StopToken,
        ahead: bool,
    ) -> Insert {
        may_block("waiting for room in the frame cache");
        let mut g = self.lock();
        let mut waited = None;
        let r = loop {
            if g.closed || stop.is_stopped() {
                break Insert::Stopped;
            }
            if ahead && let Some(at) = g.tell_resync(stream) {
                break Insert::Resync { at };
            }
            if let Some(r) = g.offer(stream, &frame) {
                break r;
            }
            if waited.is_none() {
                g.waits += 1;
                let wait = g.waits;
                g.waiting.insert(
                    stream,
                    Waiter {
                        time: frame.time,
                        ahead,
                        stop: stop.clone(),
                        wait,
                    },
                );
                waited = Some(wait);
            }
            if g.backstop() {
                // The other member hears of it as it wakes.
                self.changed.notify_all();
                continue;
            }
            g = self
                .changed
                .wait_timeout(g, WAIT_SLICE)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        };
        if let Some(wait) = waited
            && g.waiting.get(&stream).is_some_and(|w| w.wait == wait)
        {
            g.waiting.remove(&stream);
        }
        drop(g);
        self.changed.notify_all();
        r
    }

    /// [`insert`](Self::insert) without waiting for room:
    /// [`Insert::Full`] instead. Still takes the lock.
    #[track_caller]
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
    #[track_caller]
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

    /// Pin `frame`, of stream `stream`: hold it, whatever the playhead
    /// and the budget say, until [`unpin`](Self::unpin), the stream's
    /// next pin, or [`clear`](Self::clear). It goes in if it isn't held,
    /// past the window too; a frame held at its time from another run
    /// stays, and is pinned. [`Insert::Stopped`], pinning nothing, if
    /// `stop` says to stop (read under the lock, as
    /// [`try_insert_until`](Self::try_insert_until) does) or the cache is
    /// closed. The stream's frame pinned before is let go.
    #[track_caller]
    pub fn pin(&self, stream: usize, frame: Arc<VideoFrame>, stop: &StopToken) -> Insert {
        let mut g = self.lock();
        if g.closed || stop.is_stopped() {
            return Insert::Stopped;
        }
        g.pins.insert(stream, frame.time);
        // Never `None`: a pinned frame is let in past the window.
        let r = g.offer(stream, &frame).unwrap_or(Insert::Full);
        drop(g);
        self.changed.notify_all();
        r
    }

    /// Let stream `stream`'s frame at `time` go as the rules say, if it is
    /// the one pinned, evicting down to the budget.
    #[track_caller]
    pub fn unpin(&self, stream: usize, time: Time) {
        let g = self.lock();
        self.changed(g, |s| {
            if s.pinned(stream, time) {
                s.pins.remove(&stream);
                s.shrink();
            }
        });
    }

    /// Move the playhead to clip time `at`, which wakes a decode worker
    /// waiting for room.
    #[track_caller]
    pub fn set_playhead(&self, at: Time) {
        let g = self.lock();
        self.changed(g, |s| {
            s.playhead = at;
            s.rebase();
        });
    }

    /// [`set_playhead`](Self::set_playhead) if the lock is free; false
    /// when it isn't and nothing changed, to try again next frame. Never
    /// waits: for the UI thread.
    pub fn try_set_playhead(&self, at: Time) -> bool {
        match self.try_lock() {
            Some(g) => {
                self.changed(g, |s| {
                    s.playhead = at;
                    s.rebase();
                });
                true
            }
            None => false,
        }
    }

    /// [`set_playhead`](Self::set_playhead) for request `request` (a seek
    /// or a step) of stream `stream`, on its way. For a member of a
    /// locked pair's group, the group base is the playhead from the
    /// first such move of a request newer than any before until every
    /// member has landed or failed it ([`landed`](Self::landed)), so that
    /// each member's landing goes in without waiting for room.
    #[track_caller]
    pub fn seek_playhead(&self, stream: usize, request: u64, at: Time) {
        let g = self.lock();
        self.changed(g, |s| {
            s.playhead = at;
            if let Some(g) = &mut s.group
                && g.members.contains(&stream)
                && request > g.request
            {
                g.request = request;
                // A member may have landed it before the group was set.
                let pending = g
                    .members
                    .map(|m| s.landed.get(&m).is_none_or(|&l| l < request));
                g.hold = Some(Hold::Request {
                    id: request,
                    pending,
                });
            }
            s.rebase();
        });
    }

    /// Stream `stream` landed or failed request `request`, or (`None`)
    /// failed the backstop's seek: once every member of a locked pair's
    /// group has, its base is worked out from the frames held again.
    #[track_caller]
    pub fn landed(&self, stream: usize, request: Option<u64>) {
        let g = self.lock();
        self.changed(g, |s| {
            if let Some(r) = request {
                let l = s.landed.entry(stream).or_default();
                *l = (*l).max(r);
            }
            s.done(stream, request);
            s.rebase();
        });
    }

    /// The playhead the cache's rules run from: the
    /// [group base](Self::group_base) while a group is set, else the time
    /// the presenter or a request gave.
    #[track_caller]
    pub fn playhead(&self) -> Time {
        let g = self.lock();
        g.group.as_ref().map_or(g.playhead, |gr| gr.base)
    }

    /// Lock streams `members` together from now on, or no longer with
    /// `None`: a locked pair's group (M3 plan section 9), set at open.
    /// The cache's rules for the members (each one's current frame, its
    /// window, eviction and back-pressure) and [`playhead`](Self::playhead),
    /// which the upload thread starts each wanted list from, then run
    /// from the [group base](Self::group_base) instead of the time the
    /// presenter gives, so that each member's frame there is never
    /// evicted and the faster lens decodes at most its window past it.
    /// The same members again keep the group as it is.
    #[track_caller]
    pub fn set_group(&self, members: Option<[usize; 2]>) {
        let g = self.lock();
        self.changed(g, |s| s.set_group(members));
    }

    /// [`set_group`](Self::set_group) if the lock is free; false when it
    /// isn't and nothing changed. Never waits: for the UI thread.
    pub fn try_set_group(&self, members: Option<[usize; 2]>) -> bool {
        match self.try_lock() {
            Some(g) => {
                self.changed(g, |s| s.set_group(members));
                true
            }
            None => false,
        }
    }

    /// The group base Tg, while a group is set: the largest time at or
    /// before the presenter's playhead at which every member holds a
    /// frame, worked out again whenever a frame goes in or the playhead
    /// moves; the playhead itself from a seek or a step until every
    /// member has landed or failed, and whenever no such time exists;
    /// and the backstop's time while its seek is on its way
    /// ([`Insert::Resync`]). `None` without a group.
    #[track_caller]
    pub fn group_base(&self) -> Option<Time> {
        self.lock().group.as_ref().map(|g| g.base)
    }

    /// Hold at most `bytes` from now on, evicting down to it.
    #[track_caller]
    pub fn set_budget(&self, bytes: u64) {
        let g = self.lock();
        self.changed(g, |s| {
            s.budget = bytes;
            s.shrink();
        });
    }

    /// Hold at most `cap` frames with a decoder's surface a stream from
    /// now on, evicting down to it; `None` for no cap. A cap below 2 is 2:
    /// the current frame and the next.
    #[track_caller]
    pub fn set_surface_cap(&self, cap: Option<usize>) {
        let g = self.lock();
        self.changed(g, |s| {
            s.surface_cap = cap.map(|c| c.max(2));
            s.shrink();
        });
    }

    /// This cache with [`set_surface_cap`](Self::set_surface_cap)'s cap,
    /// before it is shared: it takes no lock, so the UI thread can make
    /// one.
    pub fn with_surface_cap(mut self, cap: Option<usize>) -> Self {
        let s = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        s.surface_cap = cap.map(|c| c.max(2));
        self
    }

    /// Keep frames as far as `ahead` past the playhead.
    #[track_caller]
    pub fn set_ahead(&self, ahead: Time) {
        let g = self.lock();
        self.changed(g, |s| s.ahead = ahead);
    }

    /// Drop every frame and link: a new file.
    #[track_caller]
    pub fn clear(&self) {
        let g = self.lock();
        self.changed(g, |s| {
            s.frames.clear();
            s.surfaces.clear();
            s.links.clear();
            s.frame_bytes.clear();
            s.pins.clear();
            s.bytes = 0;
        });
    }

    /// Drop stream `stream`'s links, keeping its frames: for a new
    /// decoder on the stream, whose runs are numbered afresh.
    #[track_caller]
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
    #[track_caller]
    pub fn close(&self) {
        let g = self.lock();
        self.changed(g, |s| s.closed = true);
    }

    /// Wake everyone waiting, to look at their stop tokens: after a seek
    /// has stopped the ones before it.
    #[track_caller]
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
    #[track_caller]
    pub fn get(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        self.lock().get(stream, time)
    }

    /// The frame shown at `at`, when the cache can tell: the latest at or
    /// before `at`, if the frame linked after it is later than `at`, so
    /// that no frame lies between. A frame whose successor isn't held
    /// can't be told from one followed by frames not decoded yet.
    #[track_caller]
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
    #[track_caller]
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
    #[track_caller]
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
    #[track_caller]
    pub fn next_after(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        self.lock().linked(stream, time, 1)
    }

    /// The frame before the one of `stream` at `time`, by its run links.
    #[track_caller]
    pub fn prev_before(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        self.lock().linked(stream, time, -1)
    }

    /// The last frame reached from the one of `stream` at `time` through
    /// run links: where a decoder goes on from after a seek into frames
    /// held already. `None` when no frame is held at `time`.
    #[track_caller]
    pub fn run_end(&self, stream: usize, time: Time) -> Option<Arc<VideoFrame>> {
        let g = self.lock();
        let mut at = g.get(stream, time)?;
        while let Some(next) = g.linked(stream, at.time, 1) {
            at = next;
        }
        Some(at)
    }

    /// Bumped by every change: compare it to see whether anything did.
    #[track_caller]
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Wait until the generation isn't `seen`, or `timeout` has passed,
    /// and return it. Blocks: for the upload thread, waiting for a frame
    /// to upload.
    #[track_caller]
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

    #[track_caller]
    pub fn stats(&self) -> CacheStats {
        self.lock().stats()
    }

    /// [`stats`](Self::stats) if the lock is free, `None` when it isn't,
    /// to show the last ones again. Never waits: for the UI thread.
    pub fn try_stats(&self) -> Option<CacheStats> {
        self.try_lock().map(|g| g.stats())
    }

    /// The spans of clip time stream `stream`'s frames cover, in order,
    /// for the scrub bar's shading: frames that meet (each starting where
    /// the one before it ends, to within `gap`) make one span, and a frame
    /// without a duration covers its instant. `None` when the lock is
    /// taken. Never waits: for the UI thread.
    pub fn try_ranges(&self, stream: usize, gap: Time) -> Option<Vec<(Time, Time)>> {
        let g = self.try_lock()?;
        let mut out: Vec<(Time, Time)> = Vec::new();
        for (&(_, t), e) in g.stream(stream) {
            let end = e
                .frame
                .duration
                .and_then(|d| t.checked_add(d).ok())
                .unwrap_or(t);
            match out.last_mut() {
                Some((_, last)) if t.checked_sub(gap).is_ok_and(|s| s <= *last) => {
                    *last = (*last).max(end);
                }
                _ => out.push((t, end)),
            }
        }
        Some(out)
    }

    /// Every frame held, with its stream, in stream and time order: for
    /// tests.
    #[track_caller]
    pub fn held(&self) -> Vec<(usize, Arc<VideoFrame>)> {
        self.lock()
            .frames
            .iter()
            .map(|(&(s, _), e)| (s, e.frame.clone()))
            .collect()
    }
}
