//! Shared by the engine's tests: one lavapipe device per test binary, a
//! player on a backend, and waits that fail instead of hanging.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use spherecut_core::Clock;
use spherecut_media::backend::MediaBackend;
use spherecut_playback::{Player, PlayerConfig, PlayerParts, PlayerSnapshot, Presented};
use spherecut_render::{AdapterChoice, Gpu};

/// The CPU adapter, as every Tier 1 test renders on: made once, shared by
/// the binary's tests.
pub fn gpu() -> &'static Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    GPU.get_or_init(|| {
        Gpu::headless(&AdapterChoice::Cpu)
            .expect("lavapipe, Mesa's Vulkan CPU driver, is needed for the engine tests")
    })
}

/// A player on `backend` and `clock`, on lavapipe, with doctor's checks
/// left unrun (the backends here aren't probed for them).
pub fn player(
    backend: impl MediaBackend + 'static,
    clock: Arc<dyn Clock>,
    config: PlayerConfig,
) -> Player {
    Player::new(
        PlayerParts::new(Arc::new(backend), gpu().clone(), clock),
        config,
    )
    .unwrap()
}

/// How long a test waits for a player's threads at its end. The plan
/// gives each thread 2 s; under load the upload thread finishes its pass
/// on lavapipe and then waits up to 1 s for the GPU, and with a build
/// and busy loops beside it 2 s for them all was too little.
pub const JOIN: Duration = Duration::from_secs(10);

/// Shut `player` down, and fail, saying what is left, unless every
/// thread ends within [`JOIN`] and none panicked.
pub fn join(player: Player) {
    let report = player.join(JOIN);
    assert!(report.is_clean(), "{report:?}");
}

/// `f`'s first `Some`, asked every millisecond for as long as `timeout`.
pub fn until<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(t) = f() {
            return Some(t);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A snapshot of what the threads report now: `snapshot` gives the copy
/// from before when a thread holds the status lock, which a test must not
/// take for the state now. Never waits on a lock, so it does on a thread
/// marked as the UI thread too.
pub fn snapshot(player: &Player) -> PlayerSnapshot {
    until(Duration::from_secs(20), || {
        let s = player.snapshot();
        s.fresh.then_some(s)
    })
    .expect("no snapshot read the status in 20 s")
}

/// The pts of each stream's frame on screen.
pub fn pts(p: &Presented) -> Vec<i64> {
    p.frames.iter().map(|f| f.frame.pts).collect()
}

/// The p-th percentile of `samples` (0 to 100), by nearest rank.
pub fn percentile(samples: &[Duration], p: f64) -> Duration {
    let mut v = samples.to_vec();
    v.sort();
    if v.is_empty() {
        return Duration::ZERO;
    }
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v[rank.clamp(1, v.len()) - 1]
}
