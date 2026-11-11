//! The open (M3 plan section 9): a pair locks only when its lenses'
//! timing tables agree, read through `MediaBackend::track_timing`
//! whatever the decoder; otherwise it is refused by the field that
//! differs and plays unlocked.
//!
//! Most pairs here are two of a `PerStream`'s lenses, opened one after
//! another on one player, each open clearing what the one before left,
//! which keeps the tests inside their share of the suite's budget. The
//! GOP fault pair is a file from M3.1's writer, read through
//! `FfmpegBackend`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spherecut_core::{Clock, FrameRate, ManualClock};
use spherecut_media::backend::{FfmpegBackend, HwKind, MediaBackend};
use spherecut_playback::lock::field;
use spherecut_playback::{
    CacheBudget, DecoderPreference, LockSpec, LockStatus, Player, PlayerConfig, PlayerEvent,
};
use spherecut_synth::fake::Gate;
use spherecut_synth::media::{PAIR_64_GOP_FAULT, PAIR_64_MATCHED, pairs_64, write_pair};
use spherecut_testkit::control::must_fail;

use crate::common::per_stream::{Lens, PerStream};
use crate::common::{self, pts, snapshot, until};
use crate::{WAIT, timed};

/// What `PerStream` opens: any name will do.
const LENSES: &str = "lenses.synthetic";

/// What the player said of a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    Refused(&'static str),
    Short { stream: usize, frames: u64 },
}

/// A player of `backend` on a paused `ManualClock`, decoding as
/// `decoder` asks, on three GPU slots: too few for two streams' frames
/// on screen and their new ones at once, so that an unlocked open shows
/// each stream's frame as it comes.
fn player(backend: impl MediaBackend + 'static, decoder: DecoderPreference) -> Player {
    let clock = Arc::new(ManualClock::new(FrameRate::FPS_30.frame_duration()));
    common::player(
        backend,
        clock,
        PlayerConfig {
            cache: CacheBudget::Bytes(64 << 20),
            gpu_slots: 3,
            decoder,
            ..PlayerConfig::default()
        },
    )
}

/// What `events` said of the lock.
fn of_lock(events: Vec<PlayerEvent>) -> Vec<Said> {
    events
        .into_iter()
        .filter_map(|e| match e {
            PlayerEvent::LockRefused { field } => Some(Said::Refused(field)),
            PlayerEvent::LensShort { stream, frames } => Some(Said::Short { stream, frames }),
            _ => None,
        })
        .collect()
}

/// What the player said of the lock in its events since the last call.
fn said(player: &Player) -> Vec<Said> {
    of_lock(player.events())
}

/// The lock's status once it has formed or been refused, failing after
/// [`WAIT`].
fn settled(player: &Player) -> LockStatus {
    until(WAIT, || {
        let lock = snapshot(player).lock?;
        (lock.status != LockStatus::Pending).then_some(lock.status)
    })
    .unwrap_or_else(|| panic!("the lock never settled: {:?}", snapshot(player).lock))
}

/// Present until `n` streams show a frame, failing after [`WAIT`]; their
/// pts.
fn shown(player: &Player, n: usize) -> Vec<i64> {
    until(WAIT, || {
        let p = player.present(Instant::now())?;
        (p.frames.len() == n).then(|| pts(&p))
    })
    .unwrap_or_else(|| {
        panic!(
            "{n} streams never showed: {:?}",
            player.present(Instant::now()).map(|p| pts(&p))
        )
    })
}

/// How an open of `streams` of `path` locked by `spec` settled, and what
/// the player said of its lock, once `n` streams show their first frames.
/// The worker that opens second says what came of the group before its
/// first frame lands, so by the time both show it has been said. A
/// member that fails says so just after the status shows the lock
/// refused, which the other lens may show before, so a refusal said of
/// nothing yet is waited for.
fn opened(
    player: &Player,
    path: &Path,
    streams: [usize; 2],
    spec: LockSpec,
    n: usize,
) -> (LockStatus, Vec<Said>) {
    said(player);
    player.open_locked(path.to_path_buf(), streams, spec);
    let status = settled(player);
    shown(player, n);
    let now = said(player);
    if !now.is_empty() || status == LockStatus::Locked {
        return (status, now);
    }
    let later = until(WAIT, || Some(said(player)).filter(|s| !s.is_empty()));
    (status, later.unwrap_or_default())
}

/// How an open of `streams` of `path` locked stood once its player
/// stream 0's first frame had failed to decode or gone up in a slot,
/// while `gate` held stream 1's open, and what the player said of its
/// lock once `gate` let stream 1 open and `n` streams show. A refusal
/// said of nothing yet is waited for, as in [`opened`].
fn decoded_first(
    player: &Player,
    path: &Path,
    streams: [usize; 2],
    gate: &Gate,
    n: usize,
) -> (LockStatus, Vec<Said>) {
    said(player);
    player.open_locked(path.to_path_buf(), streams, LockSpec::default());
    let epoch = snapshot(player).epoch;
    let mut heard = Vec::new();
    until(WAIT, || {
        let events = player.events();
        let failed = events
            .iter()
            .any(|e| matches!(e, PlayerEvent::DecodeFailed { stream: 0, .. }));
        heard.extend(of_lock(events));
        let up = player
            .slots()
            .views()
            .iter()
            .any(|v| v.frame.epoch == epoch && v.frame.stream == 0 && v.frame.pts == 0);
        (failed || up).then_some(())
    })
    .expect("stream 0's first frame neither failed nor went up");
    let held = snapshot(player).lock.expect("a locked open's group").status;
    gate.release();
    let status = settled(player);
    shown(player, n);
    heard.extend(said(player));
    if heard.is_empty() && status != LockStatus::Locked {
        heard = until(WAIT, || Some(said(player)).filter(|s| !s.is_empty())).unwrap_or_default();
    }
    (held, heard)
}

/// The check: the field a pair was refused by, said once. A pair that
/// locked, or that was refused without saying so once, is the error,
/// with what was said.
fn refused(
    (status, said): (LockStatus, Vec<Said>),
) -> Result<&'static str, (LockStatus, Vec<Said>)> {
    match (status, said.as_slice()) {
        (LockStatus::Refused { field }, [Said::Refused(f)]) if *f == field => Ok(field),
        _ => Err((status, said)),
    }
}

/// Opened locked, the pair shows nothing while its tables and decoders
/// are being read (`LockStatus::Pending`, lens 1's open held by a gate),
/// and both lenses together once it has locked.
#[test]
fn nothing_of_the_pair_shows_until_it_locks() {
    let _time = timed("nothing_of_the_pair_shows_until_it_locks");
    let (held, held_too) = (Gate::new(), Gate::new());
    let backend = PerStream::new(vec![
        Lens::default(),
        Lens::default().held_by(&held),
        Lens::default().held_by(&held_too),
    ]);
    let player = player(backend, DecoderPreference::Software);
    // The check: nothing presented once lens 0's first frame is up in a
    // slot, nor for a few presents more, while the other lens's open is
    // held; the error is what was.
    let nothing_shows = |player: &Player, epoch: u64| -> Result<(), Vec<i64>> {
        let up = until(WAIT, || {
            if let Some(p) = player.present(Instant::now()) {
                return Some(Err(pts(&p)));
            }
            let views = player.slots().views();
            views
                .iter()
                .any(|v| v.frame.epoch == epoch && v.frame.stream == 0 && v.frame.pts == 0)
                .then_some(Ok(()))
        });
        up.expect("lens 0's first frame never went up")?;
        for _ in 0..20 {
            if let Some(p) = player.present(Instant::now()) {
                return Err(pts(&p));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    };
    player.open_locked(LENSES.into(), [0, 1], LockSpec::default());
    nothing_shows(&player, snapshot(&player).epoch).unwrap();
    let lock = snapshot(&player).lock.map(|l| l.status);
    assert_eq!(lock, Some(LockStatus::Pending));
    held.release();
    assert_eq!(settled(&player), LockStatus::Locked);
    assert_eq!(shown(&player, 2), [0, 0]);
    assert_eq!(said(&player), []);

    // Control: opened unlocked, on three slots, lens 0 shows alone while
    // lens 2's open is held.
    player.open_streams(LENSES.into(), vec![Some(0), Some(2)]);
    let alone = must_fail(
        "opened unlocked",
        nothing_shows(&player, snapshot(&player).epoch),
    );
    assert_eq!(alone, [0]);
    assert_eq!(snapshot(&player).lock, None);
    held_too.release();
    common::join(player);
}

/// Planted faults through `PerStream`'s tables (start pts apart by more
/// than `LockSpec.offset`, rates, sizes, `stts` and `stss` that differ,
/// the last as GOPs of 8 and 10, and no tables) are each refused by the
/// field that differs, with one `LockRefused`, and the lenses play
/// unlocked; an offset equal to the planted start locks.
#[test]
fn each_planted_fault_is_refused_by_name() {
    let _time = timed("each_planted_fault_is_refused_by_name");
    // A frame of the default clip, at the 90 kHz its tables tick at.
    let frame_ticks = 3000;
    let faults: [(&str, Lens, &str); 7] = [
        (
            "starts a frame later",
            Lens::default().plant(|t| t.first_pts = frame_ticks),
            field::START,
        ),
        (
            "is at 25 fps",
            Lens::default().rate(FrameRate::FPS_25),
            field::RATE,
        ),
        ("has no tables", Lens::default().no_tables(), field::TIMING),
        ("has GOPs of 10", Lens::default().keyint(10), field::STSS),
        (
            "ticks at 30 kHz",
            Lens::default().plant(|t| {
                t.timescale = 30_000;
                t.stts = vec![(30, 1000)];
            }),
            field::TIME_BASE,
        ),
        (
            "is half the size",
            Lens::default().size(32, 16),
            field::SIZE,
        ),
        (
            "holds a frame twice as long",
            Lens::default().plant(|t| t.stts = vec![(10, 3000), (1, 6000), (19, 3000)]),
            field::STTS,
        ),
    ];
    // Lens 0 is the default clip, GOPs of 8, and lens 1 its twin; the
    // faults follow.
    let mut lenses = vec![Lens::default(), Lens::default()];
    lenses.extend(faults.iter().map(|(_, lens, _)| lens.clone()));
    let player = player(PerStream::new(lenses), DecoderPreference::Software);
    let path = Path::new(LENSES);
    let with = |k: usize, spec: LockSpec| opened(&player, path, [0, k], spec, 2);
    let mut named = Vec::new();
    for (k, (what, _, field)) in (2..).zip(&faults) {
        let got = refused(with(k, LockSpec::default()))
            .unwrap_or_else(|e| panic!("the lens that {what}: {e:?}"));
        assert_eq!(got, *field, "the lens that {what}");
        named.push(got);
    }
    // Every field the lenses can differ in, each by its own fault.
    let mut fields: Vec<&str> = field::ALL
        .into_iter()
        .filter(|f| ![field::OPEN, field::SLOTS].contains(f))
        .collect();
    named.sort_unstable();
    fields.sort_unstable();
    assert_eq!(named, fields);

    // Controls: the matched pair locks, and so does the late start with
    // its frame given as the offset.
    let (status, said) = must_fail("the matched pair", refused(with(1, LockSpec::default())));
    assert_eq!((status, said), (LockStatus::Locked, Vec::new()));
    let offset = LockSpec {
        offset: FrameRate::FPS_30.frame_to_time(1).unwrap(),
        ..LockSpec::default()
    };
    let (status, said) = must_fail("the late start with its offset", refused(with(2, offset)));
    assert_eq!((status, said), (LockStatus::Locked, Vec::new()));
    common::join(player);
}

/// A member whose open fails before the group forms, or whose first
/// frame fails to decode while the other's open is held, refuses the
/// lock with field `"open"`, and the other lens plays unlocked.
#[test]
fn a_member_that_cant_open_refuses_the_lock() {
    let _time = timed("a_member_that_cant_open_refuses_the_lock");
    let held = Gate::new();
    let backend = PerStream::new(vec![
        Lens::default(),
        Lens::default().open_fails(),
        Lens::default(),
        Lens::default().read_error_at(0),
        Lens::default().held_by(&held),
    ]);
    let player = player(backend, DecoderPreference::Software);
    let path = Path::new(LENSES);
    // Lens 0 shows alone, unlocked.
    let got = refused(opened(&player, path, [0, 1], LockSpec::default(), 1));
    assert_eq!(got, Ok(field::OPEN));
    assert_eq!(shown(&player, 1), [0]);
    // Control: both open, and the pair locks.
    let (status, said) = must_fail(
        "both open",
        refused(opened(&player, path, [0, 2], LockSpec::default(), 2)),
    );
    assert_eq!((status, said), (LockStatus::Locked, Vec::new()));

    // Lens 3 opens but fails to decode (`Worker::fail`, not the open's
    // failure) while lens 4's open is held: the group is refused before
    // lens 4 opens, and lens 4 then shows alone.
    let got = refused(decoded_first(&player, path, [3, 4], &held, 1));
    assert_eq!(got, Ok(field::OPEN));
    assert_eq!(shown(&player, 1), [0]);
    // Control: lens 0 in lens 3's place decodes, the group waits for lens
    // 4, and the pair locks once it opens.
    held.close();
    let (status, said) = must_fail(
        "decoding",
        refused(decoded_first(&player, path, [0, 4], &held, 2)),
    );
    assert_eq!((status, said), (LockStatus::Pending, Vec::new()));
    assert_eq!(settled(&player), LockStatus::Locked);
    common::join(player);
}

/// A member whose first frame fails to decode once the pair has locked
/// refuses the lock with `"open"` too (V1): it handed out no frame for
/// the pair to end at, so the group lets go of its lenses and the other
/// plays on alone. Forced, lens 1's open is held until lens 0 has
/// opened, so that lens 1's open locks the pair before its first frame
/// fails; unforced, either lens may open first. Its control, a lens that
/// decodes in lens 1's place, stays locked and the pair plays.
#[test]
fn a_member_that_fails_its_first_frame_once_locked_refuses_the_lock() {
    let _time = timed("a_member_that_fails_its_first_frame_once_locked_refuses_the_lock");
    let (gate, gate_too) = (Gate::new(), Gate::new());
    let backend = PerStream::new(vec![
        Lens::default(),
        Lens::default().read_error_at(0).held_by(&gate),
        Lens::default().read_error_at(0),
        Lens::default().held_by(&gate_too),
    ]);
    let clock = crate::held::clock();
    let player = common::player(backend, clock.clone(), crate::held::config());
    // How lens 0 opened with lens `k` locked stood, lens `k`'s open held
    // by `gate` (if any) until lens 0 has opened: what was said of the
    // lock, and what shows once played to frame 10.
    let run = |k: usize, gate: Option<&Gate>| -> ((LockStatus, Vec<Said>), Vec<i64>) {
        player.pause();
        said(&player);
        player.open_locked(LENSES.into(), [0, k], LockSpec::default());
        if let Some(gate) = gate {
            let first = until(WAIT, || {
                snapshot(&player).streams.first()?.opened.then_some(())
            });
            assert!(first.is_some(), "lens 0 never opened");
            gate.release();
        }
        let up = until(WAIT, || {
            let p = player.present(Instant::now())?;
            (pts(&p).first() == Some(&0)).then_some(())
        });
        assert!(up.is_some(), "lens 0's first frame never showed");
        player.play();
        clock.set(crate::held::at(10), Instant::now());
        let played = until(WAIT, || {
            let p = player.present(Instant::now())?;
            let shown = pts(&p);
            (shown.first() == Some(&10)).then_some(shown)
        })
        .expect("lens 0 never played to frame 10");
        let status = snapshot(&player).lock.map(|l| l.status);
        let mut heard = said(&player);
        if heard.is_empty() && status != Some(LockStatus::Locked) {
            heard =
                until(WAIT, || Some(said(&player)).filter(|s| !s.is_empty())).unwrap_or_default();
        }
        ((status.expect("a locked open's group"), heard), played)
    };
    let (forced, shown) = run(1, Some(&gate));
    assert_eq!(refused(forced), Ok(field::OPEN));
    assert_eq!(shown, [10]);
    let (unforced, shown) = run(2, None);
    assert_eq!(refused(unforced), Ok(field::OPEN));
    assert_eq!(shown, [10]);
    // Control: lens 3 decodes, and the pair stays locked and plays.
    let (locked, shown) = run(3, Some(&gate_too));
    let (status, heard) = must_fail("lens 3 decodes", refused(locked));
    assert_eq!((status, heard), (LockStatus::Locked, Vec::new()));
    assert_eq!(shown, [10, 10]);
    common::join(player);
}

/// Lens 1 a frame short locks (the count isn't compared), and the player
/// says so once, with `LensShort` naming the stream and its frames.
#[test]
fn a_lens_a_frame_short_locks_and_says_so() {
    let _time = timed("a_lens_a_frame_short_locks_and_says_so");
    let backend = PerStream::new(vec![
        Lens::default(),
        Lens::default().frames(29),
        Lens::default(),
    ]);
    let player = player(backend, DecoderPreference::Software);
    let path = Path::new(LENSES);
    // The check: a pair that locked and said once which lens is short,
    // and by how many frames.
    let short = |(status, said): (LockStatus, Vec<Said>)| match (status, said.as_slice()) {
        (LockStatus::Locked, [Said::Short { stream, frames }]) => Ok((*stream, *frames)),
        _ => Err((status, said)),
    };
    let got = short(opened(&player, path, [0, 1], LockSpec::default(), 2));
    assert_eq!(got, Ok((1, 1)));
    // Opened the other way round, the short lens is player stream 0.
    let got = short(opened(&player, path, [1, 0], LockSpec::default(), 2));
    assert_eq!(got, Ok((0, 1)));
    // Control: 30 frames and 30 lock and say nothing.
    let (status, said) = must_fail(
        "30 frames and 30",
        short(opened(&player, path, [0, 2], LockSpec::default(), 2)),
    );
    assert_eq!((status, said), (LockStatus::Locked, Vec::new()));
    common::join(player);
}

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A 2×64² two-track MP4 from M3.1's writer with the GOP fault is refused
/// by `"stss"` through `FfmpegBackend::track_timing`, so the file path is
/// tested too.
#[test]
fn the_gop_fault_file_is_refused_through_ffmpeg() {
    let _time = timed("the_gop_fault_file_is_refused_through_ffmpeg");
    let dir = TempDir(
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("lock-open-pairs-{}", std::process::id())),
    );
    let written = |name: &str| {
        let spec = pairs_64().into_iter().find(|p| p.name == name).unwrap();
        write_pair(&spec, &dir.0).unwrap()
    };
    let (fault, matched) = (written(PAIR_64_GOP_FAULT), written(PAIR_64_MATCHED));
    let player = player(FfmpegBackend::new(), DecoderPreference::Software);
    let got = refused(opened(&player, &fault, [0, 1], LockSpec::default(), 2));
    assert_eq!(got, Ok(field::STSS));
    // Control: the matched pair locks.
    let (status, said) = must_fail(
        "the matched pair",
        refused(opened(&player, &matched, [0, 1], LockSpec::default(), 2)),
    );
    assert_eq!((status, said), (LockStatus::Locked, Vec::new()));
    common::join(player);
}

/// The open reads both lenses' tables once each through the backend
/// under every decoder preference, Software included, where no probe
/// runs.
#[test]
fn timing_is_read_whatever_the_decoder() {
    let _time = timed("timing_is_read_whatever_the_decoder");
    let backend = PerStream::pair(Lens::default(), Lens::default());
    let counts = backend.clone();
    let player = player(backend, DecoderPreference::Auto);
    let path = Path::new(LENSES);
    // The tables each lens's last open read.
    let mut before = [0, 0];
    let mut reads = || {
        let now = [counts.timing_calls(0), counts.timing_calls(1)];
        let read = [now[0] - before[0], now[1] - before[1]];
        before = now;
        read
    };
    for decoder in [
        DecoderPreference::Auto,
        DecoderPreference::Software,
        DecoderPreference::Only(HwKind::VaApi),
    ] {
        player.set_config(PlayerConfig {
            decoder,
            ..player.config()
        });
        let (status, _) = opened(&player, path, [0, 1], LockSpec::default(), 2);
        assert_eq!(status, LockStatus::Locked, "{decoder:?}");
        assert_eq!(reads(), [1, 1], "{decoder:?}");
    }
    // Control: opened unlocked, nothing is read.
    player.open_streams(path.to_path_buf(), vec![Some(0), Some(1)]);
    shown(&player, 2);
    let read = reads();
    must_fail("opened unlocked", read == [1, 1]);
    assert_eq!(read, [0, 0]);
    common::join(player);
}
