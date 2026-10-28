//! The seam slice 6's audio fills (M1 plan sections 8.1, 8.4, 9.2 and
//! 9.3, DECISIONS D34).
//!
//! Until then the player runs video alone, on the clock it was given: a
//! `SystemClock` while playing for real, a `ManualClock` in tests and
//! M2's export. With an [`AudioOutputFactory`], the player opens the
//! file's audio when it opens the file, on its open worker, and then
//! follows the output's clock, the audio device's, which is what makes
//! the DAC the master (plan section 9.3: video repeats or drops frames
//! to follow it, so there is no drift to correct). Playing, pausing and
//! seeking go through that [`Clock`]: an output's `set` is its seek,
//! which restarts audio at the new time (the ring's epoch, section 9.2).
//! Preroll after a seek ends when the video target is ready and the
//! output says [`AudioOutput::ready`] for [`PREROLL_AUDIO`].

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use spherecut_core::{Clock, StopToken};

use crate::player::PlayerError;

/// How much audio from the seek's time must be queued before playback
/// resumes after a seek (plan section 8.4, step 6).
pub const PREROLL_AUDIO: Duration = Duration::from_millis(100);

/// Opens a file's audio for playback.
pub trait AudioOutputFactory: Send + Sync + std::fmt::Debug {
    /// The audio of `path`, ready to play on its own clock, paused at 0;
    /// `Ok(None)` when the file has none, or none that can be played, so
    /// the player keeps its own clock. Blocks: the player calls it on its
    /// open worker, never the UI thread.
    fn open(
        &self,
        path: &Path,
        stop: &StopToken,
    ) -> Result<Option<Arc<dyn AudioOutput>>, PlayerError>;
}

/// A file's audio, playing out.
pub trait AudioOutput: Send + Sync + std::fmt::Debug {
    /// The clock video follows while this plays. Its `set` restarts the
    /// audio at that time, and its `play` and `pause` start and stop it.
    fn clock(&self) -> Arc<dyn Clock>;
    /// Whether at least `ahead` of audio from the clock's time is queued
    /// to play. Never waits: the UI thread asks it every frame of a
    /// preroll.
    fn ready(&self, ahead: Duration) -> bool;
}
