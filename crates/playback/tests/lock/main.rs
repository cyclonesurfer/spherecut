//! Frame-locked pairs (M3 plan section 9, DECISIONS D67), on lavapipe and
//! `ManualClock`s, through the [`PerStream`] backend, whose lenses differ
//! in frames, keyframes, size, rate, decode cost and timing tables:
//!
//! - `open`: a pair opened with `Player::open_locked` locks only when its
//!   lenses' tables agree, read whatever the decoder, and is refused by
//!   the field that differs, or plays to its shorter lens and says so;
//! - `pick`: `pick_pair` shows the newest time both lenses hold, both
//!   slots or neither, and the counters say what was shown;
//! - `base`: the cache runs from the group base, which seeks, the group's
//!   catch-up, the end and the backstop keep where a pair exists;
//! - `play`: a locked pair plays on six slots without a mixed pair,
//!   paced to its slowest lens, and unlocks below its slot floor or ends
//!   when a member fails.
//!
//! Budget (M3 plan 4.1 rule 5, section 19.6): 10 s for the suite, which
//! each test prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../common/mod.rs"]
mod common;

mod base;
mod open;
mod pick;
mod play;

use std::path::Path;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use spherecut_core::{StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    MediaBackend, NextFrame, SeekMode, SeekOutcome, VideoDecode, VideoRequest,
};
use spherecut_playback::{Landed, Player, PlayerSnapshot};
use spherecut_synth::fake::{Gate, SyntheticBackend, frame_index};
use spherecut_testkit::control::must_fail;

use common::per_stream::{Lens, PerStream};

const BUDGET_S: f64 = 10.0;

/// How long a test waits for the player or a decoder, as the engine's
/// tests do.
const WAIT: Duration = Duration::from_secs(20);

/// How long a control's wait sees every worker waiting for room in the
/// cache with nothing new before it calls the player stuck: in place of
/// [`WAIT`], for a control that waits for what never comes.
const STUCK: Duration = Duration::from_millis(1500);

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// How a test waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// For as long as [`WAIT`]: a check.
    Full,
    /// For as long as [`WAIT`], or until the player is stuck
    /// ([`STUCK`]): a control.
    Stuck,
}

/// Why a wait gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GaveUp {
    /// For [`STUCK`] straight, every live worker waited for room in the
    /// cache while no stream handed out a frame or landed and the group
    /// base stayed where it was.
    Stuck,
    /// [`WAIT`] passed.
    TimedOut,
}

/// `f`'s first `Some`, asked every millisecond, waiting as `wait` says,
/// or why it never came.
fn wait_for<T>(player: &Player, wait: Wait, mut f: impl FnMut() -> Option<T>) -> Result<T, GaveUp> {
    let start = Instant::now();
    let mut still: Option<(Instant, Progress)> = None;
    loop {
        if let Some(t) = f() {
            return Ok(t);
        }
        if start.elapsed() >= WAIT {
            return Err(GaveUp::TimedOut);
        }
        if wait == Wait::Stuck {
            let s = player.snapshot();
            if s.fresh {
                match (progress(&s), &still) {
                    (Some(now), Some((since, seen))) if now == *seen => {
                        if since.elapsed() >= STUCK {
                            return Err(GaveUp::Stuck);
                        }
                    }
                    (Some(now), _) => still = Some((Instant::now(), now)),
                    (None, _) => still = None,
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// What the player has done so far: each stream's frames handed out and
/// last landing, and the group base.
type Progress = (Vec<(u64, Option<Landed>)>, Option<Time>);

/// The player's [`Progress`] while every live worker waits for room in
/// the cache; `None` while one doesn't.
fn progress(s: &PlayerSnapshot) -> Option<Progress> {
    let live = s
        .streams
        .iter()
        .filter(|x| x.opened && x.error.is_none())
        .count();
    let waiting = s.cache?.waiting;
    (live > 0 && waiting >= live).then(|| {
        let streams = s.streams.iter().map(|x| (x.handed_out, x.landed));
        (streams.collect(), s.lock.and_then(|l| l.base))
    })
}

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the suite's budget. The tests run
/// side by side, so none of them can time the rest: the last line
/// printed is the suite's time.
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

/// A request for video stream `stream`.
fn request(stream: usize) -> VideoRequest {
    VideoRequest {
        stream: Some(stream),
        ..VideoRequest::default()
    }
}

/// The frame a decoder hands out next, by the index it carries.
fn next(d: &mut dyn VideoDecode) -> Result<Option<u64>, MediaError> {
    Ok(match d.next_frame(&StopToken::never())? {
        NextFrame::Frame(f) => frame_index(&f),
        NextFrame::End | NextFrame::Stopped => None,
    })
}

/// `PerStream` opens the lens `req.stream` names, as that stream, with
/// its own clip, tables and faults, and counts what it is asked. Its
/// control: a bare `SyntheticBackend` opens its one clip as stream 0
/// whatever stream is asked for.
#[test]
fn per_stream_opens_the_stream_asked_for() {
    let _time = timed("per_stream_opens_the_stream_asked_for");
    let gate = Gate::new();
    let backend = PerStream::new(vec![
        Lens::default(),
        Lens::default()
            .frames(29)
            .size(32, 16)
            .read_error_at(20)
            .plant(|t| t.first_pts = 3000),
        Lens::default().no_tables().held_by(&gate),
        Lens::default().open_fails(),
    ]);
    let path = Path::new("lenses.synthetic");
    // What stream `s` opens as: its index, size and frames.
    let opened = |b: &dyn MediaBackend, s: usize| {
        let d = b.open_video(path, &request(s)).map_err(|e| e.to_string())?;
        let info = d.info();
        Ok::<_, String>((info.index, info.size, info.frame_count))
    };
    let opens_lens_1 = |b: &dyn MediaBackend| opened(b, 1) == Ok((1, (32, 16), Some(29)));
    assert_eq!(opened(&backend, 0), Ok((0, (64, 32), Some(30))));
    assert!(opens_lens_1(&backend), "{:?}", opened(&backend, 1));
    let none = opened(&backend, 4).unwrap_err();
    assert!(none.contains("video"), "{none}");
    let failed = opened(&backend, 3).unwrap_err();
    assert!(
        failed.ends_with("stream 3: a planted open failure"),
        "{failed}"
    );

    // Lens 1's decoder starts at pts 0 whatever its planted start, and
    // reads to its read error and no further; a seek before it still
    // works.
    let mut d = backend.open_video(path, &request(1)).unwrap();
    assert_eq!(d.info().origin_pts, 0);
    for k in 0..20 {
        assert_eq!(next(&mut *d).unwrap(), Some(k));
    }
    for _ in 0..2 {
        let e = next(&mut *d).unwrap_err().to_string();
        assert!(
            e.ends_with("stream 1: a planted read error at frame 20"),
            "{e}"
        );
    }
    let at = backend.lens(1).clip.rate.frame_to_time(5).unwrap();
    match d.seek(at, SeekMode::Exact, &StopToken::never()).unwrap() {
        SeekOutcome::Frame { frame, .. } => assert_eq!(frame_index(&frame), Some(5)),
        SeekOutcome::Stopped => panic!("stopped"),
    }

    // Each lens's tables: derived from its clip, planted, or none.
    assert_eq!(
        backend.track_timing(path, 0).unwrap(),
        Some(Lens::default().derived_tables())
    );
    let planted = backend.track_timing(path, 1).unwrap().unwrap();
    assert_eq!((planted.count, planted.first_pts), (29, 3000));
    assert_eq!(backend.track_timing(path, 2).unwrap(), None);
    assert!(backend.track_timing(path, 4).is_err());

    // Lens 2's open waits for its gate.
    let (tx, rx) = mpsc::channel();
    let held = backend.clone();
    let opener = std::thread::spawn(move || {
        let d = held.open_video(Path::new("lenses.synthetic"), &request(2));
        tx.send(d.map(|d| d.info().index).map_err(|e| e.to_string()))
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    gate.release();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Ok(2));
    opener.join().unwrap();

    // Every call counted, failed ones too, a stream past the lenses not.
    let counts: Vec<_> = (0..4)
        .map(|s| (backend.opens(s), backend.timing_calls(s)))
        .collect();
    assert_eq!(counts, [(1, 1), (2, 1), (1, 1), (1, 0)]);

    // Control: a bare `SyntheticBackend` of lens 1's clip opens it as
    // stream 0, and its stream 1 is its stream 0.
    let bare = SyntheticBackend::new(backend.lens(1).clip.clone());
    must_fail("a bare SyntheticBackend", opens_lens_1(&bare));
    assert_eq!(opened(&bare, 1), opened(&bare, 0));
}
