//! The headless playback engine (M1 plan section 8, DECISIONS D34): the
//! bounded frame cache between the decode workers and the upload thread,
//! the budget it is sized by, and the numbers the performance overlay,
//! the smoke run and the benchmarks read. The workers, GPU slots and the
//! presenter that the GUI's viewer and M2's export drive come next.
//!
//! No UI type is in here: the GUI, `spherecut render` and the Tier 2
//! benchmarks drive the same engine, the last two with no window
//! (DECISIONS D28).

#![forbid(unsafe_code)]

pub mod budget;
pub mod cache;
pub mod decoder;
pub mod perf;

pub use budget::{AutoBudget, BudgetError, CacheBudget, auto_budget, read_auto_budget};
pub use cache::{CacheStats, FrameCache, Insert, MIN_FRAMES};
pub use decoder::{DecoderChoice, DecoderPreference, HwDecodeCheck, HwSupport, StreamFormat};
pub use perf::{Counter, PerfSnapshot, PerfStats, Stage, StageSummary};
