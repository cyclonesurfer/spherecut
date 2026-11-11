//! Play (M3 plan section 9): a locked pair never shows a mixed pair, by
//! tag or by content, paces to its slowest lens, asks for six slots and
//! unlocks below its floor of two a member, and ends when a member
//! fails.
//!
//! The content check reads both slots of a present back from the GPU
//! ([`read_texture`]): `SyntheticBackend` writes each frame's index into
//! its first 8 luma bytes, so the check sees what the GPU holds whatever
//! the slots' tags say.

use std::collections::HashMap;
use std::ffi::OsString;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use spherecut_core::{Clock, FrameRate, ManualClock, Rounding, Time};
use spherecut_media::backend::SeekMode;
use spherecut_playback::{
    CacheBudget, Counter, DecoderPreference, LockSpec, LockStatus, LockTest, Player, PlayerConfig,
    PlayerEvent, Presented, ShownFrame, SlotFrame,
};
use spherecut_render::readback::read_texture;
use spherecut_testkit::control::must_fail;

use crate::common::pacing::{self, Freeze, Pace, Sample, Unpaced};
use crate::common::per_stream::{Hold, Lens, PerStream};
use crate::common::{self, pts, snapshot};
use crate::{GaveUp, WAIT, Wait, timed, wait_for};

/// What `PerStream` opens: any name will do.
const LENSES: &str = "lenses.synthetic";

const RATE: FrameRate = FrameRate::FPS_30;

/// The bytes of a frame of the default clip: 64x32, 4:2:0, 8-bit.
const FRAME: u64 = 64 * 32 * 3 / 2;

fn at(k: i64) -> Time {
    RATE.frame_to_time(k).unwrap()
}

/// The frame shown at clip time `t`.
fn frame_of(t: Time) -> i64 {
    RATE.time_to_frame(t, Rounding::Floor).unwrap()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A player of `backend` on a paused `ManualClock`, decoding in software,
/// holding `budget` bytes of frames, on `slots` GPU slots (a locked
/// pair takes six whatever this says).
fn player(backend: PerStream, slots: u8, budget: u64) -> (Player, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let config = PlayerConfig {
        cache: CacheBudget::Bytes(budget),
        gpu_slots: slots,
        decoder: DecoderPreference::Software,
        ..PlayerConfig::default()
    };
    (common::player(backend, clock.clone(), config), clock)
}

/// Held by the pacing test and by the mixed run, so that the two never
/// run at once. Their players share lavapipe's one device: the mixed
/// run's three players, reading back every pair they show, held up the
/// pacing test's uploads by more than the present the pacing rule gives
/// a frame to go up. Side by side, 3 runs of the suite in 10 failed the
/// rule; apart, none in 20.
fn alone() -> MutexGuard<'static, ()> {
    static ALONE: Mutex<()> = Mutex::new(());
    ALONE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A spec with `test`'s switches.
fn spec(test: LockTest) -> LockSpec {
    LockSpec {
        test,
        ..LockSpec::default()
    }
}

/// Move a running `clock` on a frame for each `period` passed since
/// `moved`, as far as frame `to`: play at a frame every `period`, which
/// passes frames over when the presents come slower. A stopped clock
/// (a preroll, the end) moves on from where it starts again.
fn tick(clock: &ManualClock, moved: &mut Instant, period: Duration, to: i64) {
    if !clock.is_running() {
        *moved = Instant::now();
        return;
    }
    let periods = moved.elapsed().as_nanos() / period.as_nanos().max(1);
    let frames = u32::try_from(periods).unwrap_or(u32::MAX);
    let k = frame_of(clock.now());
    if frames > 0 && k < to {
        clock.set(at((k + i64::from(frames)).min(to)), Instant::now());
        *moved += period * frames;
    }
}

/// Open streams 0 and 1, locked by `spec` or, with `None`, unlocked as
/// `open_streams` opens them, and present until both show their first
/// frames: locked, the lock has formed by then. Each present goes
/// through `check`.
fn open(player: &Player, spec: Option<LockSpec>, check: &mut Mismatches) {
    let status = open_as(player, spec, check);
    assert_eq!(status, spec.map(|_| LockStatus::Locked));
}

/// [`open`], whatever becomes of the lock: its status once both lenses
/// show their first frames.
fn open_as(player: &Player, spec: Option<LockSpec>, check: &mut Mismatches) -> Option<LockStatus> {
    match spec {
        Some(spec) => player.open_locked(LENSES.into(), [0, 1], spec),
        None => player.open_streams(LENSES.into(), vec![Some(0), Some(1)]),
    }
    let began = Instant::now();
    while !check.present(player).is_some_and(|p| pts(&p) == [0, 0]) {
        assert!(began.elapsed() < WAIT, "the pair never showed");
        std::thread::sleep(ms(1));
    }
    snapshot(player).lock.map(|l| l.status)
}

/// The index of the frame `f`'s slot holds, read back from the GPU.
fn held(f: &ShownFrame) -> i64 {
    let texels = read_texture(common::gpu(), f.gpu.plane_texture(0).unwrap(), 0).unwrap();
    i64::try_from(u64::from_le_bytes(texels.data[..8].try_into().unwrap())).unwrap()
}

/// What the presents a test checked showed: how many showed frames, how
/// many of those showed the lenses apart by their slots' tags (one lens
/// alone, or the two at different times), and how many showed both
/// lenses apart by what their slots hold, read back (the two apart, or
/// either not the frame its tag names), with the first present that did
/// either.
#[derive(Debug, Default)]
struct Mismatches {
    presents: u64,
    by_tag: u64,
    by_content: u64,
    first: Option<String>,
    /// The pts shown by the last present that showed both lenses.
    shown: Option<[i64; 2]>,
    /// What the last present's slots were read back as: each slot, its
    /// GPU frame's address, the frame its tag names, and the index it
    /// held. A slot is read back once for each frame it shows, as a slot
    /// Shown is never written.
    read: Vec<(usize, usize, SlotFrame, i64)>,
}

impl Mismatches {
    /// Check present `p`: both lenses' frames at one clip time, and both
    /// slots holding that frame.
    fn check(&mut self, p: &Presented) {
        if p.frames.is_empty() {
            return;
        }
        self.presents += 1;
        let tags = pts(p);
        let read: Vec<_> = p
            .frames
            .iter()
            .map(|f| {
                let key = (f.slot, Arc::as_ptr(&f.gpu).addr(), f.frame);
                let before = self.read.iter().find(|r| (r.0, r.1, r.2) == key);
                (key.0, key.1, key.2, before.map_or_else(|| held(f), |r| r.3))
            })
            .collect();
        let held: Vec<i64> = read.iter().map(|r| r.3).collect();
        self.read = read;
        let by_tag = !matches!(tags[..], [a, b] if a == b);
        let by_content = matches!(held[..], [a, b] if a != b) || held != tags;
        self.by_tag += u64::from(by_tag);
        self.by_content += u64::from(by_content);
        if (by_tag || by_content) && self.first.is_none() {
            self.first = Some(format!(
                "present {}: tags {tags:?}, slots holding {held:?}",
                self.presents
            ));
        }
        if let [a, b] = tags[..] {
            self.shown = Some([a, b]);
        }
    }

    /// Present on `player` now, checking what it showed.
    fn present(&mut self, player: &Player) -> Option<Presented> {
        let p = player.present(Instant::now());
        if let Some(p) = &p {
            self.check(p);
        }
        p
    }

    /// The check: no present showed the lenses apart, by tag or by
    /// content, and the player counted none unpaired (`unpaired`). The
    /// presents that showed frames.
    fn unmixed(&self, unpaired: u64) -> Result<u64, String> {
        if self.by_tag == 0 && self.by_content == 0 && unpaired == 0 {
            Ok(self.presents)
        } else {
            Err(format!(
                "{} of {} presents apart by tag, {} by content, {unpaired} counted unpaired; \
                 the first: {:?}",
                self.by_tag, self.presents, self.by_content, self.first
            ))
        }
    }
}

/// A seed's stream of numbers (SplitMix64), for a run that can be
/// replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `lo..hi`.
    fn within(&mut self, lo: i64, hi: i64) -> i64 {
        let span = u64::try_from(hi - lo).unwrap().max(1);
        lo + i64::try_from(self.next() % span).unwrap()
    }
}

/// The mixed run's clip, in frames: longer than the cache's window of
/// 1 s ahead (30 frames).
const MIXED_FRAMES: i64 = 120;

/// The seeds of the mixed run, and the actions each takes after its
/// seek forward while lens 0 leads.
const SEEDS: u64 = 64;
const ACTIONS: usize = 3;

/// One seed's run of the mixed test on a player, its presents checked,
/// lens 1 behind `hold`.
struct Mixed<'a> {
    player: &'a Player,
    clock: &'a ManualClock,
    hold: &'a Hold,
    seed: u64,
    seen: &'a mut Mismatches,
}

impl Mixed<'_> {
    /// Present until both lenses show frames that `want` takes (by pts),
    /// failing after `WAIT` with `what` and the player's state.
    fn lands(&mut self, what: &str, want: impl Fn([i64; 2]) -> bool) {
        let began = Instant::now();
        loop {
            let p = self.seen.present(self.player);
            if p.and_then(|p| <[i64; 2]>::try_from(pts(&p)).ok())
                .is_some_and(&want)
            {
                return;
            }
            assert!(
                began.elapsed() < WAIT,
                "seed {}: {what} never landed: showing {:?}, {:?}",
                self.seed,
                self.seen.shown,
                snapshot(self.player)
            );
            std::thread::sleep(ms(1));
        }
    }

    /// Seek to frame `k` by `mode`: an exact seek lands both lenses on
    /// `k`, a keyframe seek on its keyframe or, held in the cache, `k`.
    fn seek(&mut self, k: i64, mode: SeekMode) {
        let key = match mode {
            SeekMode::Exact => k,
            SeekMode::Keyframe => k - k % 8,
        };
        self.player.seek(at(k), mode);
        self.lands(&format!("a {mode:?} seek to {k}"), |got| {
            got.iter().all(|&f| f == k || f == key)
        });
    }

    /// Step `n` frames from each lens's frame on screen.
    fn step(&mut self, n: i32) {
        let from = self.seen.shown.expect("a frame on screen");
        let want = from.map(|f| (f + i64::from(n)).clamp(0, MIXED_FRAMES - 1));
        self.player.step(n);
        self.lands(&format!("a step of {n} from {from:?}"), |got| got == want);
    }

    /// Play `frames` on, the clock a frame every `period`, or to the end.
    fn play(&mut self, frames: i64, period: Duration) {
        self.player.play();
        let to = (frame_of(self.clock.now()) + frames).min(MIXED_FRAMES);
        let began = Instant::now();
        let mut moved = Instant::now();
        while !(self.clock.is_running() && frame_of(self.clock.now()) >= to) {
            tick(self.clock, &mut moved, period, to);
            self.seen.present(self.player);
            if snapshot(self.player).at_end {
                return;
            }
            assert!(
                began.elapsed() < WAIT,
                "seed {}: play never reached frame {to}: {:?}",
                self.seed,
                snapshot(self.player)
            );
            std::thread::sleep(ms(1));
        }
    }

    /// Seek forward while lens 0 leads lens 1, to a frame past every one
    /// either lens holds and more than 1 s past the pair on screen: the
    /// seek after which the old pair is still the newest time both hold,
    /// and lens 0's landing lies past its window from it. Lens 1 is held
    /// at frame 1 from the open, so lens 0 leads once it has decoded two
    /// frames, whatever the machine's load; the hold lets go once the seek
    /// is asked for.
    fn lead(&mut self, rng: &mut Rng) {
        let began = Instant::now();
        loop {
            self.seen.present(self.player);
            let s = snapshot(self.player);
            let newest = [0, 1].map(|i| s.streams[i].newest.map(frame_of));
            if let ([Some(n0), Some(n1)], Some([shown, _])) = (newest, self.seen.shown)
                && n0 > n1
            {
                let past = (n0.max(n1) + 2).max(shown + 31);
                assert!(
                    past < MIXED_FRAMES,
                    "seed {}: lens 0 led to {n0}",
                    self.seed
                );
                let k = rng.within(past, MIXED_FRAMES);
                self.player.seek(at(k), SeekMode::Exact);
                self.hold.release(i64::MAX);
                self.lands(&format!("a seek to {k} while lens 0 led"), |got| {
                    got == [k, k]
                });
                return;
            }
            assert!(
                began.elapsed() < WAIT,
                "seed {}: lens 0 never led: {:?}",
                self.seed,
                s.streams
            );
            std::thread::sleep(ms(1));
        }
    }

    /// The seed's run: open with lens 1 held at frame 1, the seek forward
    /// while lens 0 leads, then [`ACTIONS`] random seeks, steps, plays and
    /// pauses, the plays at a frame every 1, 2 or 4 ms.
    fn run(&mut self, lock: Option<LockSpec>) {
        let mut rng = Rng(self.seed);
        self.hold.release(1);
        open(self.player, lock, self.seen);
        self.lead(&mut rng);
        for _ in 0..ACTIONS {
            match rng.within(0, 6) {
                0 => self.seek(rng.within(0, MIXED_FRAMES), SeekMode::Exact),
                1 => self.seek(rng.within(0, MIXED_FRAMES), SeekMode::Keyframe),
                2 => {
                    let n = i32::try_from(rng.within(1, 4)).unwrap();
                    self.step(if rng.within(0, 2) == 0 { n } else { -n });
                }
                3 | 4 => {
                    let period = ms(1 << rng.within(0, 3));
                    self.play(rng.within(4, 12), period);
                }
                _ => {
                    self.player.pause();
                    self.seen.present(self.player);
                }
            }
        }
        self.player.pause();
    }
}

/// Lens 1 2.5 to 3 times slower than lens 0, jittered, 64 seeds of
/// random seeks, plays, steps, pauses and rates: at every present both
/// slots are read back and hold the same frame, the one their tags name,
/// and after every seek and step both lenses land within `WAIT`. Its
/// control, the same run through `open_streams`, counts mismatches by
/// tag and by content.
#[test]
fn a_locked_pair_never_shows_mismatched_frames() {
    let _alone = alone();
    let _time = timed("a_locked_pair_never_shows_mismatched_frames");
    // What the runs of `seeds` showed, locked or not, on a player of
    // their own, the presents it counted unpaired, and the seeds run: all
    // of them, or until `enough`. Lens 0 at 0.2 ms a frame, and lens 1 at
    // 0.5 ms with up to 0.1 ms more drawn for each frame from its seed
    // and open: lens 1 at 2.5 to 3 times lens 0's cost, and behind a
    // hold until each seed's lead. The player is opened again for each
    // seed, on three slots: locked, it takes six.
    let runs = |lock: Option<LockSpec>, seeds: Range<u64>, enough: fn(&Mismatches) -> bool| {
        let lens = |cost: u64| {
            Lens::default()
                .frames(MIXED_FRAMES as u64)
                .cost(Duration::from_micros(cost))
        };
        let hold = Hold::at(i64::MAX);
        let lens1 = lens(500)
            .jitter(seeds.end, Duration::from_micros(100))
            .hold(&hold);
        let backend = PerStream::pair(lens(200), lens1);
        let (player, clock) = player(backend, 3, 64 << 20);
        let mut seen = Mismatches::default();
        let mut run = 0;
        for seed in seeds {
            Mixed {
                player: &player,
                clock: &clock,
                hold: &hold,
                seed,
                seen: &mut seen,
            }
            .run(lock);
            run += 1;
            if enough(&seen) {
                break;
            }
        }
        let unpaired = player.perf().count(Counter::UnpairedPresents);
        common::join(player);
        (seen, unpaired, run)
    };
    // Half the seeds on each of two players at once, and the control
    // beside them: unlocked, lens 0's frames show before lens 1's come,
    // by the slots' tags and by what they hold, and its seeds run until
    // both are seen.
    let lock = Some(LockSpec::default());
    let both = |s: &Mismatches| s.by_tag > 0 && s.by_content > 0;
    let [first, second, unlocked] = std::thread::scope(|s| {
        [
            s.spawn(|| runs(lock, 0..SEEDS / 2, |_| false)),
            s.spawn(|| runs(lock, SEEDS / 2..SEEDS, |_| false)),
            s.spawn(|| runs(None, 0..SEEDS, both)),
        ]
        .map(|run| run.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    });
    for (seen, unpaired, seeds) in [first, second] {
        let presents = seen.unmixed(unpaired).unwrap();
        eprintln!("{seeds} seeds, {presents} presents checked, none apart");
    }

    // Control: unlocked.
    let (seen, unpaired, seeds) = unlocked;
    let apart = must_fail("the lenses unlocked", seen.unmixed(unpaired));
    eprintln!("unlocked, over {seeds} seeds: {apart}");
    assert!(both(&seen) && unpaired > 0, "{apart}");
}

/// A case of the pacing test: its lenses, its clock, and its cache of
/// `budget` frames (a window of a quarter of them a lens: half of it,
/// shared by the two), halved once the clock passes the clip's middle if
/// `halve`. `slowest` is the most its slowest lens costs a frame, and
/// `behind` says that lens is slower than the clock, so that it hands
/// out only what its catch-ups land on.
struct Case {
    name: &'static str,
    backend: PerStream,
    pace: Pace,
    budget: u64,
    halve: bool,
    slowest: Duration,
    behind: bool,
}

impl Case {
    /// The longest a pair of the case may stand still after a present
    /// that put it up `lag` frames behind the clock, in the clock's
    /// frames: as long as its slowest lens takes to finish the frame in
    /// hand and decode its way to where the clock was then, which a
    /// catch-up to the clock takes, or the leading lens's window where
    /// that is longer; then `TRAIL` more, and the most a present held up
    /// moves the clock (`MOST_A_PRESENT`), for the present that puts the
    /// pair up. A pair whose lenses all trail the clock (the leading one
    /// held back by its window, once the pair is a window behind) catches
    /// up to it, decoding every frame it passes over, and lands as far
    /// behind as the catch-up took: below real time, its freezes grow by
    /// its slowest lens's cost over the frame period at each catch-up, as
    /// an unlocked lens's do. A pair whose freezes grow faster outgrows
    /// the bound, and one that freezes at the clock outgrows it at once.
    fn longest_freeze(&self, lag: i64) -> i64 {
        let window = i64::try_from(self.budget / 4).unwrap();
        let decode = self.slowest.as_nanos() * u128::try_from(lag.max(0) + 1).unwrap();
        let frames = i64::try_from(decode.div_ceil(self.pace.period.as_nanos())).unwrap();
        frames.max(window) + pacing::TRAIL + pacing::MOST_A_PRESENT
    }

    /// The frames of the case's clock its slowest lens's decoder waited
    /// for a CPU through `freeze` (`decoding`, by sample), which the
    /// freeze may last on top of [`Case::longest_freeze`]: on a CPU busy
    /// beyond the tests, each frame the lens decodes takes its cost and
    /// the wait to be woken once that has passed, so a catch-up takes
    /// longer than its cost says. On a CPU that isn't busy it is next to
    /// nothing.
    fn waited(&self, freeze: &Freeze, decoding: &[Duration]) -> i64 {
        let at = |i: usize| decoding.get(i).copied().unwrap_or_default();
        let waited = at(freeze.until).saturating_sub(at(freeze.at));
        i64::try_from(waited.as_nanos().div_ceil(self.pace.period.as_nanos())).unwrap()
    }
}

/// What a run of the pacing test saw: its samples, what its presents
/// showed, the pair resyncs (the cache's own count, which takes each at
/// once) and the unpaired presents counted, the presents after the
/// warm-up whose pair wasn't up a present on, the frames lens 1
/// decoded after the warm-up, catch-ups' included, and how long its
/// decoder had waited for a CPU by each sample.
struct Played {
    samples: Vec<Sample>,
    seen: Mismatches,
    resyncs: u64,
    unpaired: u64,
    late: u64,
    decoded: u64,
    decoding: Vec<Duration>,
}

/// Four cases: on a 90-frame clip whose clock moves a frame every 3 ms,
/// lens 1 at 2.5 to 3 times the frame period, both lenses slower than
/// the clock, and lens 1 slow with the budget halved mid-play; and on a
/// 60-frame clip whose clock moves a frame every 6 ms, lens 1 slower
/// than lens 0 but within the clock, so that it hands out every frame.
/// After the first 30 frames of each, the pacing rule holds, the share
/// judged over the four together ([`pacing::pooled`]) on 20 frames handed
/// out or more; no pair stands still longer than its slowest lens takes
/// to catch up to where the clock was as it went up
/// ([`Case::longest_freeze`]); the presents whose pair wasn't up a
/// present on are at most one, or a quarter of the frames handed out,
/// over the three slow cases and in the fourth ([`settled`]); and no
/// pair resyncs but where the budget was halved, which may, though in
/// practice it doesn't (base.rs's
/// `a_shrinking_budget_mid_play_resyncs_the_pair` holds the backstop to
/// its control). The clock moves on at most two frames a present, one
/// in the fourth case, whose frames are longer than a present; a present
/// gives its pair as long again as the engine's threads waited for a
/// CPU, and a freeze as long again as lens 1's decoder did
/// ([`Case::waited`]), so that neither a test thread held up nor a CPU
/// busy beyond the tests is counted against the engine. The controls:
/// the group base and the group `until` off fails the rule by
/// trailing, and the presents late; a run that hands out a frame fewer
/// than [`pacing::JUDGED`] after the warm-up is too few to judge; the
/// fourth case's run with every third pair passed over has too few
/// pairs; and, at a slow case's own length, a pair whose freezes double
/// at the clock, and one whose catch-ups take five times its lag, fail
/// the freeze, though they keep to the rule.
#[test]
fn a_locked_pair_paces_to_its_slowest_lens() {
    let _alone = alone();
    let _time = timed("a_locked_pair_paces_to_its_slowest_lens");
    const FRAMES: i64 = 90;
    const WITHIN: i64 = 60;
    // Between presents, beside the millisecond `pacing::play` sleeps.
    const PRESENTS: Duration = Duration::from_millis(3);
    // A frame every `period` ms, on a clip of `frames`: at most two a
    // present while a frame is shorter than one, else one.
    let pace = |frames: i64, period: u64| Pace {
        rate: RATE,
        period: ms(period),
        frames,
        most: if ms(period) < PRESENTS + ms(1) {
            pacing::MOST_A_PRESENT
        } else {
            1
        },
    };
    let lens = |frames: i64, cost_us: u64| {
        Lens::default()
            .frames(frames as u64)
            .cost(Duration::from_micros(cost_us))
    };
    // Lens 1 at 7.5 to 9 ms a frame, lens 0 at once; both slower than
    // the clock, at 4.5 and 9 ms; lens 1 at 4.5 ms against a clock at 6.
    let one_slow = || {
        let lens1 = lens(FRAMES, 7500).jitter(3, Duration::from_micros(1500));
        PerStream::pair(lens(FRAMES, 0), lens1)
    };
    let slow = |name, backend, halve| Case {
        name,
        backend,
        pace: pace(FRAMES, 3),
        budget: 64,
        halve,
        slowest: ms(9),
        behind: true,
    };
    let within = Case {
        name: "lens 1 slower, within the clock",
        backend: PerStream::pair(lens(WITHIN, 0), lens(WITHIN, 4500)),
        pace: pace(WITHIN, 6),
        budget: 24,
        halve: false,
        slowest: Duration::from_micros(4500),
        behind: false,
    };
    let budget = |frames: u64| PlayerConfig {
        cache: CacheBudget::Bytes(frames * FRAME),
        gpu_slots: 6,
        decoder: DecoderPreference::Software,
        ..PlayerConfig::default()
    };
    // A run of `case`'s lenses, locked by `test`.
    let run = |case: &Case, test: LockTest| {
        let (player, clock) = player(case.backend.clone(), 6, case.budget * FRAME);
        let mut seen = Mismatches::default();
        open(&player, Some(spec(test)), &mut seen);
        // Every slot made before the clock runs: a slot is made the first
        // time a frame goes in, which on lavapipe takes long enough to
        // hold up the first frames played.
        let full = common::until(WAIT, || {
            seen.present(&player);
            (player.slots().views().len() == 6).then_some(())
        });
        assert!(full.is_some(), "the slots never filled");
        let epoch = snapshot(&player).epoch;
        // Presents come every 4 ms or so, as from a 250 Hz display: a
        // frame handed out has a present's time to go up in its slot,
        // with its pair's, and the next lens 1 hands out, at least 4.5 ms
        // on, a present of its own. A CPU busy beyond the tests adds the
        // time the other threads waited for one since the present
        // (`starved`), which the clock leaves out. A present whose pair
        // isn't up then is counted late, and waits for it `SETTLE` more
        // at most.
        let mut halved = !case.halve;
        let mut late = 0;
        let mut warm = None;
        // How long lens 1's decoder waited for a CPU by each present, from
        // here on (`Played::decoding`).
        let decoder = Queued::named("decode-v1");
        let mut decoding = Vec::new();
        let mut samples = pacing::play(&player, &clock, case.pace, [0, 1], WAIT, |p, s| {
            decoding.push(decoder.since());
            let queued = Queued::read();
            if let Some(p) = p {
                seen.check(p);
            }
            if warm.is_none() && frame_of(s.clock) >= pacing::WARM_UP {
                warm = Some(snapshot(&player).streams[1].decoded);
            }
            if !halved && frame_of(s.clock) >= case.pace.frames / 2 {
                player.set_config(budget(case.budget / 2));
                halved = true;
            }
            std::thread::sleep(PRESENTS);
            let began = Instant::now();
            if !goes_up(&player, epoch, s, || began.elapsed() < starved(&queued)) {
                late += u64::from(frame_of(s.clock) >= pacing::WARM_UP);
                goes_up(&player, epoch, s, || {
                    began.elapsed() < SETTLE + starved(&queued)
                });
            }
            starved(&queued).min(began.elapsed())
        });
        // Play stops at the end, and the presents go on there, as a
        // window's do: until the pair shows the slowest lens's newest
        // frame as play ended, which it may have handed out on the last
        // present's heels, or for 100 ms.
        if let Some(&Sample {
            newest: [Some(n0), Some(n1)],
            ..
        }) = samples.last()
        {
            let began = Instant::now();
            let behind = |s: Option<&Sample>| s.is_some_and(|s| s.shown < Some(n0.min(n1)));
            while behind(samples.last()) && began.elapsed() < ms(100) {
                std::thread::sleep(PRESENTS);
                let p = player.present(Instant::now());
                if let Some(p) = &p {
                    seen.check(p);
                }
                samples.push(Sample::of(p.as_ref(), &snapshot(&player), [0, 1]));
                decoding.push(decoder.since());
            }
        }
        let resyncs = player.cache().stats().resyncs;
        let unpaired = player.perf().count(Counter::UnpairedPresents);
        let decoded = snapshot(&player).streams[1].decoded - warm.unwrap_or(0);
        common::join(player);
        Played {
            samples,
            seen,
            resyncs,
            unpaired,
            late,
            decoded,
            decoding,
        }
    };
    // A case's own checks: no pair apart, no resync unless the budget
    // was halved, and no freeze longer than its catch-up.
    let holds = |case: &Case, played: &Played| {
        played.seen.unmixed(played.unpaired)?;
        if played.resyncs > 0 && !case.halve {
            return Err(format!("{} resyncs", played.resyncs));
        }
        unfrozen(case, &played.samples, &played.decoding)?;
        Ok::<_, String>(())
    };
    let cases = [
        slow("lens 1 slow", one_slow(), false),
        slow(
            "both slow",
            PerStream::pair(lens(FRAMES, 4500), lens(FRAMES, 9000)),
            false,
        ),
        slow("lens 1 slow, the budget halved", one_slow(), true),
        within,
    ];
    let played: Vec<Played> = cases
        .iter()
        .map(|case| run(case, LockTest::default()))
        .collect();
    for (case, p) in cases.iter().zip(&played) {
        holds(case, p).unwrap_or_else(|e| panic!("{}: {e}", case.name));
    }
    let samples: Vec<&[Sample]> = played.iter().map(|p| &p.samples[..]).collect();
    let paced = pacing::pooled(&samples, RATE).unwrap_or_else(|e| {
        let each: Vec<String> = cases
            .iter()
            .zip(&samples)
            .map(|(c, s)| format!("{}: {:?}", c.name, pacing::pacing(s, RATE)))
            .collect();
        panic!("{e:?}; {}", each.join("; "))
    });
    for ((case, p), paced) in cases.iter().zip(&played).zip(&paced) {
        // The freezes longer than a present held up and the trail give,
        // each from where the clock was, how far behind, and its bound.
        let freezes: Vec<String> = (pacing::freezes(&p.samples, RATE).iter())
            .filter(|f| f.frames > pacing::TRAIL + pacing::MOST_A_PRESENT)
            .map(|f| {
                let most = case.longest_freeze(f.lag) + case.waited(f, &p.decoding);
                format!("{} from {}, {} behind, of {most}", f.frames, f.clock, f.lag)
            })
            .collect();
        eprintln!(
            "{}: {paced:?}, lens 1 decoding {} frames meanwhile; its pair still for [{}] frames, \
             {} resyncs, {} presents late",
            case.name,
            p.decoded,
            freezes.join("; "),
            p.resyncs,
            p.late
        );
    }
    // The presents late, over the slow cases and in the other.
    let late = |behind: bool| {
        let runs: Vec<&Played> = (cases.iter().zip(&played))
            .filter(|(c, _)| c.behind == behind)
            .map(|(_, p)| p)
            .collect();
        settled(&runs)
    };
    late(true).unwrap_or_else(|e| panic!("the slow cases: {e}"));
    late(false).unwrap_or_else(|e| panic!("{}: {e}", cases[3].name));

    // Control: the group base and the group `until` off. Lens 0 runs
    // from the clock, its frames at lens 1's times mostly gone before
    // lens 1 decodes them: the pair trails lens 1's newest, and the
    // presents after it find their pair not up. Its freezes end at the
    // odd pair lens 0 still holds, within the bound: the freeze's control
    // is the slide below.
    let off = LockTest {
        no_group_base: true,
        ..LockTest::default()
    };
    let control = slow("lens 1 slow, no group base or until", one_slow(), false);
    let frozen = run(&control, off);
    let e = must_fail(
        "no group base or until",
        pacing::pooled(&[&frozen.samples[..]], RATE),
    );
    assert!(matches!(e, Unpaced::Trailed { .. }), "{e:?}");
    must_fail(
        "no group base or until, the presents late",
        settled(&[&frozen]),
    );
    // Control: a run that shows a pair at every frame after the warm-up
    // hands out one frame too few to judge the share by; a frame longer,
    // it hands out enough.
    let pairs = |last: i64| sliding(&(pacing::WARM_UP..=last).map(|k| (k, k)).collect::<Vec<_>>());
    let judged = pacing::WARM_UP + i64::try_from(pacing::JUDGED).unwrap() - 1;
    let e = must_fail(
        "a run a frame too short to judge",
        pacing::pooled(&[&pairs(judged - 1)[..]], RATE),
    );
    assert_eq!(
        e,
        Unpaced::Unjudged {
            handed_out: pacing::JUDGED - 1
        }
    );
    pacing::pooled(&[&pairs(judged)[..]], RATE)
        .unwrap_or_else(|e| panic!("a run long enough to judge: {e:?}"));
    // Control: the fourth case's run as a presenter would have shown it
    // that passed every third pair over, showing the pair before: two
    // pairs in three of the frames handed out, too few.
    let skipped = skipping(&played[3].samples);
    let e = must_fail(
        "every third pair passed over",
        pacing::pooled(&[&skipped[..]], RATE),
    );
    assert!(matches!(e, Unpaced::FewPairs { .. }), "{e:?}");
    // Controls, at a slow case's own length, each pair the slowest lens's
    // newest, as the rule asks: a pair at the clock whose freezes double,
    // 4 frames of it and then 8, 16 and 32 (to the end); and one whose
    // catch-ups take five times its lag where its slowest lens takes
    // three, 8 frames behind at frame 30 and 40 frames still, then 40
    // behind.
    let slow_case = &cases[1];
    let slides = [
        (
            "a pair whose freezes double",
            vec![(30, 30), (34, 34), (42, 42), (58, 58), (90, 89)],
        ),
        (
            "a pair whose catch-ups take five times its lag",
            vec![(21, 21), (30, 22), (70, 30), (90, 89)],
        ),
    ];
    for (name, ups) in slides {
        let slide = sliding(&ups);
        pacing::pacing(&slide, RATE).unwrap_or_else(|e| panic!("{name}: the rule: {e:?}"));
        let decoding = vec![Duration::ZERO; slide.len()];
        must_fail(name, unfrozen(slow_case, &slide, &decoding));
    }
    // Control, to come with the engine's planted publish delay (unit A's
    // `LockTest::publish_delay`, written in by unit C once merged): the
    // fourth case run again with each slot published 4 ms late must fail
    // `settled`, its pair late at about one present in three.
}

/// A run of a pair that keeps up with the clock, a pair at each of its
/// frames, until the first of `ups`, and from there shows a new pair only
/// at the clock's frames `ups` name, each with the frame it shows, and
/// stands still between them: a present a frame of the clock, each lens
/// handing its frame out as the pair shows it, the slowest lens's newest,
/// to the last of `ups`, the end.
fn sliding(ups: &[(i64, i64)]) -> Vec<Sample> {
    let (first, end) = (ups[0].0, ups[ups.len() - 1].0);
    (0..=end)
        .map(|k| {
            let up = &ups[..ups.partition_point(|&(a, _)| a <= k)];
            let shown = up.last().map_or(k, |&(_, f)| f);
            let handed = u64::try_from(first.min(k + 1) + up.len() as i64).unwrap();
            Sample {
                clock: at(k),
                shown: Some(at(shown)),
                handed_out: [handed; 2],
                newest: [Some(at(shown)); 2],
                end: k == end,
            }
        })
        .collect()
}

/// `samples` as a presenter would have shown them that passed over each
/// pair of a frame a multiple of three, showing the pair before.
fn skipping(samples: &[Sample]) -> Vec<Sample> {
    let mut shown = Vec::<Sample>::with_capacity(samples.len());
    for s in samples {
        let passed = s.shown.is_some_and(|t| frame_of(t) % 3 == 0);
        let before = shown.last().and_then(|b| b.shown);
        shown.push(Sample {
            shown: if passed { before } else { s.shown },
            ..*s
        });
    }
    shown
}

/// Whether a run of `case` never stood its pair still after the warm-up
/// longer than [`Case::longest_freeze`] gives for how far behind the
/// clock it was as it went up, and [`Case::waited`] for its slowest
/// lens's decoder kept from a CPU meanwhile (`decoding`, by sample): its
/// longest freeze, or the first too long.
fn unfrozen(case: &Case, samples: &[Sample], decoding: &[Duration]) -> Result<i64, String> {
    let freezes = pacing::freezes(samples, RATE);
    let most = |f: &Freeze| case.longest_freeze(f.lag) + case.waited(f, decoding);
    match freezes.iter().find(|f| f.frames > most(f)) {
        Some(f) => Err(format!(
            "the pair stood still for {} frames of the clock from frame {}, {} behind it, more \
             than {}",
            f.frames,
            f.clock,
            f.lag,
            most(f)
        )),
        None => Ok(freezes.iter().map(|f| f.frames).max().unwrap_or(0)),
    }
}

/// Whether the presents of `runs` counted late after the warm-up (their
/// pair not up a present on, give or take [`starved`]) are at most one
/// between them, or at most a quarter
/// of the frames their slowest lenses handed out meanwhile. A pair is
/// late only once its slowest lens's newest moves past the pair in the
/// slots, about once a frame it hands out, so a pair slow to go up
/// throughout is late about as often as that, and fails; a rare stall
/// is late once.
fn settled(runs: &[&Played]) -> Result<u64, String> {
    let late = runs.iter().map(|p| p.late).sum::<u64>();
    let handed_out = (runs.iter())
        .map(|p| pacing::handed_out(&p.samples, RATE))
        .sum::<u64>();
    if late <= 1 || late * 4 <= handed_out {
        Ok(late)
    } else {
        Err(format!(
            "{late} presents after the warm-up found their pair not up a present on, against \
             {handed_out} frames handed out"
        ))
    }
}

/// How long a present of the pacing test waits, past its own 4 ms, for
/// the pair the pacing rule looks for next to go up. A catch-up hands
/// out lens 1's frame some frames on, both lenses' frames there then go
/// up at once, and on a CPU the other tests share, lavapipe took longer
/// than one present for them (2 runs of the suite in about 50, the pair
/// up one present later). A pair the lock never forms still fails the
/// rule, `SETTLE` later (and [`starved`]), and a present that waits is
/// counted late ([`settled`]).
const SETTLE: Duration = Duration::from_millis(25);

/// How long the threads of this process but the test's own waited for a
/// CPU between them since a present (`queued`), `3 * SETTLE` at most:
/// the time the present gives its pair to go up beyond its own 4 ms
/// before it counts it late, and beyond `SETTLE` before it gives up. On
/// a CPU busy beyond the tests, a thread the engine wakes waits for
/// another's time slice to end: beside four busy loops on four CPUs, its
/// threads waited 12 to 16 ms for one in a present, and without this a
/// pair went up 35 ms into its wait, too late (1 run of the pacing test
/// in about 30), and a slow case's pair was late twice against 7 frames
/// handed out (1 in 60). On a CPU that isn't busy it is a millisecond or
/// so, what the other tests' threads wait.
fn starved(queued: &Queued) -> Duration {
    queued.since().min(3 * SETTLE)
}

/// How long each thread of this process but the caller, or each named
/// with a prefix, has waited for a CPU while it could run, as the kernel
/// counts it: the second field of `/proc/self/task/<tid>/schedstat`, in
/// nanoseconds. Empty where the kernel doesn't count it.
struct Queued {
    waited: HashMap<OsString, u64>,
    named: Option<&'static str>,
}

impl Queued {
    /// Every thread but the caller.
    fn read() -> Queued {
        Queued::of(None)
    }

    /// The threads whose names begin with `prefix`, the other tests'
    /// players' among them.
    fn named(prefix: &'static str) -> Queued {
        Queued::of(Some(prefix))
    }

    fn of(named: Option<&'static str>) -> Queued {
        let me = std::fs::read_link("/proc/thread-self").ok();
        let me = me.as_deref().and_then(Path::file_name);
        let tasks = std::fs::read_dir("/proc/self/task")
            .into_iter()
            .flatten()
            .flatten();
        let read = |task: &std::fs::DirEntry, file: &str| {
            std::fs::read_to_string(task.path().join(file)).ok()
        };
        let waited = tasks
            .filter(|task| match named {
                Some(prefix) => read(task, "comm").is_some_and(|n| n.starts_with(prefix)),
                None => Some(task.file_name().as_os_str()) != me,
            })
            .filter_map(|task| {
                let stat = read(&task, "schedstat")?;
                let waited = stat.split_whitespace().nth(1)?.parse().ok()?;
                Some((task.file_name(), waited))
            })
            .collect();
        Queued { waited, named }
    }

    /// How long the threads have waited for a CPU since `self` was read,
    /// between them: a thread begun since counts all its own.
    fn since(&self) -> Duration {
        let now = Queued::of(self.named);
        let waited = (now.waited.iter())
            .map(|(task, &ns)| ns.saturating_sub(self.waited.get(task).copied().unwrap_or(0)))
            .sum::<u64>();
        Duration::from_nanos(waited)
    }
}

/// Whether the pair [`pair_up`] looks for after `s` goes up while `wait`
/// holds, looking every millisecond: at once, at least.
fn goes_up(player: &Player, epoch: u64, s: &Sample, wait: impl Fn() -> bool) -> bool {
    loop {
        if pair_up(player, epoch, s) {
            return true;
        }
        if !wait() {
            return false;
        }
        std::thread::sleep(ms(1));
    }
}

/// Whether the slots hold a pair, of open `epoch`, that the pacing rule
/// takes at the present after `s`: both lenses' frames at one time, at
/// most `s`'s clock and at most `TRAIL` frames before the slowest lens's
/// newest frame as `s` saw it. True until both lenses have handed out a
/// frame.
fn pair_up(player: &Player, epoch: u64, s: &Sample) -> bool {
    let [Some(n0), Some(n1)] = s.newest else {
        return true;
    };
    let least = frame_of(n0.min(n1).min(s.clock)) - pacing::TRAIL;
    let views = player.slots().views();
    let times = |m: usize| {
        views
            .iter()
            .filter(move |v| v.frame.epoch == epoch && v.frame.stream == m)
            .map(|v| v.frame.time)
    };
    times(0).any(|t| t <= s.clock && frame_of(t) >= least && times(1).any(|u| u == t))
}

/// A locked pair asks for six slots, whatever the setting, until the
/// next open.
#[test]
fn a_locked_pair_asks_for_six_slots() {
    let _time = timed("a_locked_pair_asks_for_six_slots");
    let backend = PerStream::pair(Lens::default(), Lens::default());
    let (player, _clock) = player(backend, 3, 64 << 20);
    // The slots in use once the lenses show, opened locked or not, as
    // the ring and the snapshot say.
    let slots = |lock: Option<LockSpec>| {
        open(&player, lock, &mut Mismatches::default());
        (player.slots().active(), snapshot(&player).slots.active)
    };
    assert_eq!(slots(Some(LockSpec::default())), (6, 6));

    // Control: unlocked, the setting holds.
    let unlocked = slots(None);
    must_fail("the lenses unlocked", unlocked == (6, 6));
    assert_eq!(unlocked, (3, 3));
    common::join(player);
}

/// The slots dropped that `events` said, and the lock's refusals by
/// field.
fn of_slots(events: Vec<PlayerEvent>) -> (usize, Vec<&'static str>) {
    let (mut dropped, mut refused) = (0, Vec::new());
    for e in events {
        match e {
            PlayerEvent::SlotDropped { .. } => dropped += 1,
            PlayerEvent::LockRefused { field } => refused.push(field),
            _ => {}
        }
    }
    (dropped, refused)
}

/// How long the floor's and the failure's controls see nothing move
/// before they call the player stuck, in place of `STUCK`. Each
/// control's player, once stuck, stays so for good: the pair on screen
/// holds every slot left, or the pair is held at a failed lens's last
/// frame. Until then the clock, a frame every 2 or 3 ms, moves the group
/// base, and the slots die before the pair sticks. A shorter wait loses
/// nothing there, and keeps `play.rs` within its share of the suite's
/// budget.
const STALL: Duration = Duration::from_millis(500);

/// What a run of the floor test saw: whether new frames came (or why
/// they never did), the slots dropped, the lock's refusals and its
/// status, and the slots dead.
type Floor = (
    Result<Vec<i64>, GaveUp>,
    usize,
    Vec<&'static str>,
    Option<LockStatus>,
    usize,
);

/// Slots killed to three, mid-play and mid-seek, unlock the pair with
/// `LockRefused { field: "slots" }` within `WAIT`, never stuck; and the
/// pair opened again on the three slots left is refused with `"slots"`
/// as it locks, and plays. Its controls, the lock without the floor, as
/// the slots die mid-play and mid-seek and in the open again, get stuck.
#[test]
fn the_slot_floor_unlocks_the_pair() {
    let _time = timed("the_slot_floor_unlocks_the_pair");
    // Play on with the clock a frame every 3 ms to frame 30, where it
    // stops, so that a pair that can't go on leaves the cache full and
    // the workers waiting for room. Whether new frames came, both lenses
    // past frame 20 or, `mid_seek`, on the seek's frame 60, waiting as
    // `wait` says; then the events since the last call and the snapshot.
    let play_on =
        |player: &Player, clock: &ManualClock, moved: &mut Instant, mid_seek: bool, wait: Wait| {
            let came = |shown: &[i64]| {
                if mid_seek {
                    shown == [60, 60]
                } else {
                    shown.len() == 2 && shown.iter().all(|&f| f > 20)
                }
            };
            let shown = wait_for(player, wait, || {
                tick(clock, moved, ms(3), 30);
                let shown = pts(&player.present(Instant::now())?);
                came(&shown).then_some(shown)
            });
            let (dropped, refused) = of_slots(player.events());
            let s = snapshot(player);
            (
                shown,
                dropped,
                refused,
                s.lock.map(|l| l.status),
                s.slots.dead,
            )
        };
    // Lenses of 150 frames, decoded at once, locked by `test`. Three slot
    // allocations fail, once play has shown frame 10, or for the uploads
    // of a seek to frame 60, planted as it goes out once the open's
    // frames are all up; then play on. With `again`, the pair is opened
    // once more on the same player, locked by its test, and played from
    // the start in the same way, waiting as its wait says: the lock's
    // status as both lenses show their first frames, and that open's run.
    let kill = |test: LockTest, mid_seek: bool, wait: Wait, again: Option<(LockTest, Wait)>| {
        let backend = PerStream::pair(Lens::default().frames(150), Lens::default().frames(150));
        let (player, clock) = player(backend, 6, 64 << 20);
        let mut seen = Mismatches::default();
        open(&player, Some(spec(test)), &mut seen);
        let mut moved = Instant::now();
        if mid_seek {
            // Planted after the seek, the failures could come once its
            // frames were up already, with nothing left to claim while
            // paused; planted while the open's frames go up, they would
            // fall on those.
            let full = common::until(WAIT, || {
                seen.present(&player);
                (player.slots().views().len() == 6).then_some(())
            });
            assert!(full.is_some(), "the slots never filled");
            player.fail_slot_allocations(3);
            player.seek(at(60), SeekMode::Exact);
        } else {
            player.play();
            let ten = common::until(WAIT, || {
                tick(&clock, &mut moved, ms(3), 10);
                seen.present(&player);
                (seen.shown == Some([10, 10])).then_some(())
            });
            assert!(ten.is_some(), "play never showed frame 10");
            player.fail_slot_allocations(3);
        }
        let first: Floor = play_on(&player, &clock, &mut moved, mid_seek, wait);
        let again = again.map(|(test, wait)| {
            let status = open_as(&player, Some(spec(test)), &mut seen);
            player.play();
            let mut moved = Instant::now();
            let run: Floor = play_on(&player, &clock, &mut moved, false, wait);
            (status, run)
        });
        common::join(player);
        (first, again)
    };
    // The checks: the slots die to three and the pair unlocks, new
    // frames coming; opened again, it is refused as it locks, and plays.
    let unlocked = |(shown, dropped, refused, status, dead): Floor, what: &str| {
        let shown = shown.unwrap_or_else(|e| panic!("{what}: {e:?}"));
        assert_eq!((dropped, dead), (3, 3), "{what}: {shown:?}");
        assert_eq!(refused, ["slots"], "{what}");
        assert_eq!(status, Some(LockStatus::Refused { field: "slots" }));
    };
    // The controls, the floor off, beside the checks: as the slots die,
    // mid-play and mid-seek, and in the open again, the pair on screen,
    // with one lens's next frame up beside it, holds every slot left.
    let no_floor = LockTest {
        no_floor: true,
        ..LockTest::default()
    };
    let [dying, seeking, reopened] = std::thread::scope(|s| {
        let dying = s.spawn(|| kill(no_floor, false, Wait::StuckFor(STALL), None));
        let seeking = s.spawn(|| kill(no_floor, true, Wait::StuckFor(STALL), None));
        let reopened = s.spawn(|| {
            kill(
                LockTest::default(),
                false,
                Wait::Stuck,
                Some((no_floor, Wait::StuckFor(STALL))),
            )
        });
        let again = Some((LockTest::default(), Wait::Stuck));
        let (first, again) = kill(LockTest::default(), false, Wait::Stuck, again);
        unlocked(first, "mid-play");
        let (status, run) = again.unwrap();
        assert_eq!(status, Some(LockStatus::Refused { field: "slots" }));
        let (shown, dropped, refused, status, dead) = run;
        let shown = shown.unwrap_or_else(|e| panic!("opened again: {e:?}"));
        assert_eq!((dropped, dead), (0, 3), "opened again: {shown:?}");
        assert_eq!(refused, ["slots"], "opened again");
        assert_eq!(status, Some(LockStatus::Refused { field: "slots" }));
        let (first, _) = kill(LockTest::default(), true, Wait::Stuck, None);
        unlocked(first, "mid-seek");
        [dying, seeking, reopened]
            .map(|c| c.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    });
    let ((shown, dropped, refused, status, _), _) = dying;
    assert_eq!(must_fail("no floor", shown), GaveUp::Stuck);
    assert_eq!(
        (dropped, refused, status),
        (3, vec![], Some(LockStatus::Locked))
    );
    let ((shown, dropped, refused, status, dead), _) = seeking;
    assert_eq!(must_fail("no floor, mid-seek", shown), GaveUp::Stuck);
    assert_eq!(
        (dropped, refused, status, dead),
        (3, vec![], Some(LockStatus::Locked), 3)
    );
    let (first, again) = reopened;
    unlocked(first, "mid-play, before the open again without the floor");
    let (status, (shown, dropped, refused, _, dead)) = again.unwrap();
    assert_eq!(must_fail("no floor, opened again", shown), GaveUp::Stuck);
    assert_eq!(
        (status, dropped, refused, dead),
        (Some(LockStatus::Locked), 0, vec![], 3)
    );
}

/// A read error on lens 1 mid-play ends the pair within `WAIT`. Its
/// control, the rule off, neither ends nor shows a new pair within
/// `WAIT`.
#[test]
fn a_member_that_fails_mid_play_ends_the_pair() {
    let _time = timed("a_member_that_fails_mid_play_ends_the_pair");
    // Lenses of 120 frames, decoded at once, lens 1's from frame 60 on
    // unreadable: it fails once its window, 1 s ahead of the pair, gets
    // there. Played with the clock moving a frame every 2 ms to frame
    // 70: the frame the clock is on once play ends, waiting as `wait`
    // says; the pair on screen then, once it is lens 1's last, or else
    // now; and whether lens 1's status says it failed.
    let end = |test: LockTest, wait: Wait| {
        let backend = PerStream::pair(
            Lens::default().frames(120),
            Lens::default().frames(120).read_error_at(60),
        );
        let (player, clock) = player(backend, 6, 64 << 20);
        open(&player, Some(spec(test)), &mut Mismatches::default());
        player.play();
        let mut moved = Instant::now();
        let ended = wait_for(&player, wait, || {
            tick(&clock, &mut moved, ms(2), 70);
            let _ = player.present(Instant::now());
            let s = snapshot(&player);
            s.at_end.then(|| frame_of(s.clock))
        });
        let shown = common::until(WAIT, || {
            let shown = pts(&player.present(Instant::now())?);
            (ended.is_err() || shown == [59, 59]).then_some(shown)
        });
        let failed = snapshot(&player).streams[1].error.is_some();
        common::join(player);
        (ended, shown, failed)
    };
    // The control, the rule off, beside the check: nothing ends the pair,
    // and no pair past lens 1's last frame can show.
    let off = LockTest {
        no_end_on_failure: true,
        ..LockTest::default()
    };
    let (check, control) = std::thread::scope(|s| {
        let control = s.spawn(|| end(off, Wait::StuckFor(STALL)));
        let check = end(LockTest::default(), Wait::Full);
        let control = control.join();
        (
            check,
            control.unwrap_or_else(|e| std::panic::resume_unwind(e)),
        )
    });
    assert_eq!(check, (Ok(59), Some(vec![59, 59]), true));

    let (ended, shown, failed) = control;
    assert_eq!(must_fail("no end on failure", ended), GaveUp::Stuck);
    let shown = shown.expect("the pair on screen");
    assert!(shown.iter().all(|&f| f <= 59) && failed, "{shown:?}");
}
