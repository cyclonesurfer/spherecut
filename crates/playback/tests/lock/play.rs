//! Play (M3 plan section 9): a locked pair never shows a mixed pair, by
//! tag or by content, paces to its slowest lens, asks for six slots and
//! unlocks below its floor of two a member, and ends when a member
//! fails.
//!
//! The content check reads both slots of a present back from the GPU
//! ([`read_texture`]): `SyntheticBackend` writes each frame's index into
//! its first 8 luma bytes, so the check sees what the GPU holds whatever
//! the slots' tags say.

use std::ops::Range;
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

use crate::common::pacing::{self, Pace, Sample};
use crate::common::per_stream::{Lens, PerStream};
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
    match spec {
        Some(spec) => player.open_locked(LENSES.into(), [0, 1], spec),
        None => player.open_streams(LENSES.into(), vec![Some(0), Some(1)]),
    }
    let began = Instant::now();
    while !check.present(player).is_some_and(|p| pts(&p) == [0, 0]) {
        assert!(began.elapsed() < WAIT, "the pair never showed");
        std::thread::sleep(ms(1));
    }
    let status = snapshot(player).lock.map(|l| l.status);
    assert_eq!(status, spec.map(|_| LockStatus::Locked));
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

/// One seed's run of the mixed test on a player, its presents checked.
struct Mixed<'a> {
    player: &'a Player,
    clock: &'a ManualClock,
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
    /// and lens 0's landing lies past its window from it. Lens 0 decodes
    /// faster, so it leads while the windows fill after a landing; a
    /// seek back to the start fills them again if this missed it.
    fn lead(&mut self, rng: &mut Rng) {
        for _ in 0..20 {
            let began = Instant::now();
            while began.elapsed() < ms(50) {
                self.seen.present(self.player);
                let s = snapshot(self.player);
                let newest = [0, 1].map(|i| s.streams[i].newest.map(frame_of));
                if let ([Some(n0), Some(n1)], Some([shown, _])) = (newest, self.seen.shown)
                    && n0 > n1
                {
                    let past = (n0.max(n1) + 2).max(shown + 31);
                    if past < MIXED_FRAMES {
                        self.seek(rng.within(past, MIXED_FRAMES), SeekMode::Exact);
                        return;
                    }
                }
                std::thread::sleep(ms(1));
            }
            self.seek(0, SeekMode::Exact);
        }
        panic!("seed {}: lens 0 never led", self.seed);
    }

    /// The seed's run: open, the seek forward while lens 0 leads, then
    /// [`ACTIONS`] random seeks, steps, plays and pauses, the plays at a
    /// frame every 1, 2 or 4 ms.
    fn run(&mut self, lock: Option<LockSpec>) {
        let mut rng = Rng(self.seed);
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

/// Lens 1 2.5 to 3 times slower than lens 0 with jitter, 64 seeds of
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
    // of them, or until `enough`. Lens 0 at 0.2 ms a frame and lens 1 at
    // 0.5 ms, each with up to 0.1 ms more drawn for each frame from its
    // seed and open: lens 1 at 2.5 to 3 times lens 0's cost. The player
    // is opened again for each seed, on three slots: locked, it takes
    // six.
    let runs = |lock: Option<LockSpec>, seeds: Range<u64>, enough: fn(&Mismatches) -> bool| {
        let lens = |cost: u64, seed: u64| {
            Lens::default()
                .frames(MIXED_FRAMES as u64)
                .cost(Duration::from_micros(cost))
                .jitter(seed, Duration::from_micros(100))
        };
        let backend = PerStream::pair(lens(200, seeds.start), lens(500, seeds.end));
        let (player, clock) = player(backend, 3, 64 << 20);
        let mut seen = Mismatches::default();
        let mut run = 0;
        for seed in seeds {
            Mixed {
                player: &player,
                clock: &clock,
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

/// On a 90-frame clip whose clock moves a frame every 3 ms, with lens 1
/// at 2.5 to 3 times the frame period, both lenses slower than the clock,
/// and the first case with the budget halved mid-play, the pacing rule
/// holds after the first 30 frames. Its control, the group base and the
/// group `until` off, freezes and fails it.
#[test]
fn a_locked_pair_paces_to_its_slowest_lens() {
    let _alone = alone();
    let _time = timed("a_locked_pair_paces_to_its_slowest_lens");
    const FRAMES: i64 = 90;
    let pace = Pace {
        rate: RATE,
        period: ms(3),
        frames: FRAMES,
    };
    // Between presents, beside the millisecond `pacing::play` sleeps.
    const PRESENTS: Duration = Duration::from_millis(3);
    let lens = |cost_us: u64| {
        Lens::default()
            .frames(FRAMES as u64)
            .cost(Duration::from_micros(cost_us))
    };
    // Lens 1 at 7.5 to 9 ms a frame, lens 0 at once; both slower than
    // the clock, at 4.5 and 9 ms.
    let one_slow = || {
        let lens1 = lens(7500).jitter(3, Duration::from_micros(1500));
        PerStream::pair(lens(0), lens1)
    };
    let both_slow = || PerStream::pair(lens(4500), lens(9000));
    // A budget of `frames` frames, a window of a quarter of them a lens
    // (half of it, shared by the two), on six slots.
    let budget = |frames: u64| PlayerConfig {
        cache: CacheBudget::Bytes(frames * FRAME),
        gpu_slots: 6,
        decoder: DecoderPreference::Software,
        ..PlayerConfig::default()
    };
    // A run of `backend` locked by `test`, on a budget of 64 frames
    // halved once the clock passes frame 45 if `halve`: its samples,
    // what its presents showed, and the resyncs and unpaired presents
    // counted.
    let run = |backend: PerStream, test: LockTest, halve: bool| {
        let (player, clock) = player(backend, 6, 64 * FRAME);
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
        // Presents come every 4 ms or so, as from a 250 Hz display: a
        // frame handed out has a present's time to go up in its slot,
        // with its pair's, and the next lens 1 hands out, at least 7.5 ms
        // on, a present of its own.
        let mut halved = !halve;
        let mut samples = pacing::play(&player, &clock, pace, [0, 1], WAIT, |p, s| {
            if let Some(p) = p {
                seen.check(p);
            }
            if !halved && frame_of(s.clock) >= 45 {
                player.set_config(budget(32));
                halved = true;
            }
            std::thread::sleep(PRESENTS);
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
            }
        }
        let counted = [Counter::PairResyncs, Counter::UnpairedPresents];
        let [resyncs, unpaired] = counted.map(|c| player.perf().count(c));
        common::join(player);
        (samples, seen, resyncs, unpaired)
    };
    // The check: the pacing rule, the content check, and no resync
    // unless the budget was halved.
    type Run = (Vec<Sample>, Mismatches, u64, u64);
    let holds = |(samples, seen, resyncs, unpaired): Run, halved: bool| {
        let paced = pacing::pacing(&samples, RATE).map_err(|e| format!("{e:?}"))?;
        seen.unmixed(unpaired)?;
        if resyncs > 0 && !halved {
            return Err(format!("{resyncs} resyncs"));
        }
        Ok::<_, String>((paced, resyncs))
    };
    for (name, backend, halve) in [
        ("lens 1 slow", one_slow(), false),
        ("both slow", both_slow(), false),
        ("lens 1 slow, the budget halved", one_slow(), true),
    ] {
        let got = holds(run(backend, LockTest::default(), halve), halve);
        let (paced, resyncs) = got.unwrap_or_else(|e| panic!("{name}: {e}"));
        eprintln!("{name}: {paced:?}, {resyncs} resyncs");
    }

    // Control: the group base and the group `until` off. Lens 0 runs
    // from the clock, its frames at lens 1's times gone before lens 1
    // decodes them: no pair shows past the first.
    let off = LockTest {
        no_group_base: true,
        ..LockTest::default()
    };
    let e = must_fail(
        "no group base or until",
        holds(run(one_slow(), off, false), false),
    );
    assert!(e.contains("Trailed"), "{e}");
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

/// Slots killed to three, mid-play and mid-seek, unlock the pair with
/// `LockRefused { field: "slots" }` within `WAIT`, never stuck. Its
/// control, the lock without the floor, times out.
#[test]
fn the_slot_floor_unlocks_the_pair() {
    let _time = timed("the_slot_floor_unlocks_the_pair");
    // Lenses of 150 frames, decoded at once. Three slot allocations fail,
    // once play has shown frame 10, the clock a frame every 3 ms, or as
    // a seek to frame 60 goes out; the clock then plays on to frame 30
    // and stops there, so that a pair that can't go on leaves the cache
    // full and the workers waiting for room. Whether new frames came,
    // both lenses past frame 20 or on the seek's frame, waiting as `wait`
    // says; the slots dropped, the lock's refusals and its status, and
    // the slots dead.
    let kill = |test: LockTest, mid_seek: bool, wait: Wait| {
        let backend = PerStream::pair(Lens::default().frames(150), Lens::default().frames(150));
        let (player, clock) = player(backend, 6, 64 << 20);
        let mut seen = Mismatches::default();
        open(&player, Some(spec(test)), &mut seen);
        let mut moved = Instant::now();
        if mid_seek {
            player.seek(at(60), SeekMode::Exact);
            player.fail_slot_allocations(3);
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
        let came = |shown: &[i64]| {
            if mid_seek {
                shown == [60, 60]
            } else {
                shown.len() == 2 && shown.iter().all(|&f| f > 20)
            }
        };
        let shown = wait_for(&player, wait, || {
            tick(&clock, &mut moved, ms(3), 30);
            let shown = pts(&player.present(Instant::now())?);
            came(&shown).then_some(shown)
        });
        let (dropped, refused) = of_slots(player.events());
        let s = snapshot(&player);
        common::join(player);
        (
            shown,
            dropped,
            refused,
            s.lock.map(|l| l.status),
            s.slots.dead,
        )
    };
    // The control, the floor off, beside the checks: the pair on screen,
    // with one lens's next frame up beside it, holds every slot left.
    let no_floor = LockTest {
        no_floor: true,
        ..LockTest::default()
    };
    let control = std::thread::scope(|s| {
        let control = s.spawn(|| kill(no_floor, false, Wait::Stuck));
        for mid_seek in [false, true] {
            let (shown, dropped, refused, status, dead) =
                kill(LockTest::default(), mid_seek, Wait::Stuck);
            let shown = shown.unwrap_or_else(|e| panic!("mid-seek {mid_seek}: {e:?}"));
            assert_eq!((dropped, dead), (3, 3), "mid-seek {mid_seek}: {shown:?}");
            assert_eq!(refused, ["slots"], "mid-seek {mid_seek}");
            assert_eq!(status, Some(LockStatus::Refused { field: "slots" }));
        }
        control
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    });
    let (shown, dropped, refused, status, _) = control;
    assert_eq!(must_fail("no floor", shown), GaveUp::Stuck);
    assert_eq!(
        (dropped, refused, status),
        (3, vec![], Some(LockStatus::Locked))
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
        let control = s.spawn(|| end(off, Wait::Stuck));
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
