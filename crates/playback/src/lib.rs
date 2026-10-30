//! The headless playback engine (M1 plan section 8, DECISIONS D34): the
//! [`Player`] that the GUI's viewer, the Tier 2 benchmarks and M2's
//! export drive; its decode workers, one per video stream; the bounded
//! frame cache between them and the upload thread, and the budget it is
//! sized by; the GPU slots the upload thread fills and the presenter
//! shows from; and the numbers the performance overlay, the smoke run
//! and the benchmarks read.
//!
//! Threads (plan section 8.1): the UI thread, which only sends commands
//! and reads with `try_lock` and atomics; a transient `open` worker per
//! file; `decode-v{i}` per stream; one `upload` thread; and, for a file
//! with audio, `audio-decode` and the output's (`audio-out`, or
//! `audio-null` with no device) from [`audio`].
//!
//! No UI type is in here: the GUI, `spherecut render` and the Tier 2
//! benchmarks drive the same engine, the last two with no window
//! (DECISIONS D28).

#![forbid(unsafe_code)]

pub mod audio;
pub mod budget;
pub mod cache;
pub mod decoder;
pub mod perf;
pub mod player;
mod shared;
pub mod slots;
mod upload;
mod worker;

pub use audio::{
    AudioDevice, AudioOutput, AudioOutputFactory, DeviceAudio, PREROLL_AUDIO, PREROLL_LIMIT,
    PlayerThreads, PlayingAudio,
};
pub use budget::{AutoBudget, BudgetError, CacheBudget, auto_budget, read_auto_budget};
pub use cache::{CacheStats, FrameCache, Insert, MIN_FRAMES};
pub use decoder::{
    DecoderChoice, DecoderPreference, HwDecodeCheck, HwKeeper, HwPromise, HwSupport, StreamFormat,
};
pub use perf::{Counter, MemoryStats, PerfSnapshot, PerfStats, Stage, StageSummary};
pub use player::{
    PREROLL_FRAMES, PlayState, Player, PlayerConfig, PlayerError, PlayerEvent, PlayerParts,
    PlayerSnapshot, Presented, SURFACE_CAP, ShownFrame, ShutdownReport, ZERO_COPY_ENV,
    ZeroCopy, zero_copy_from_env,
};
pub use shared::{BudgetStatus, Landed, Repaint, SeekCounts, StreamStatus};
pub use slots::{SlotFrame, SlotRing, SlotState, SlotStats, SlotView};
pub use upload::BUDGET_RECHECK;
