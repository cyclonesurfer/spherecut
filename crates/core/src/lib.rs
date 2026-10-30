//! Spherecut core: names, exact time and the `--time` grammar, the frame
//! and colour vocabulary, cancellation, the playback clocks, the guard
//! that keeps blocking calls off the UI thread, atomic file writes, the
//! plan of a Vulkan device shared with FFmpeg, and
//! (later) the project model, timeline, keyframes and undo.

#![forbid(unsafe_code)]

pub mod blocking;
pub mod brand;
pub mod cancel;
pub mod clock;
pub mod diag;
pub mod fs;
pub mod time;
pub mod timespec;
pub mod video;
pub mod vkplan;

pub use blocking::may_block;
pub use cancel::{StopSource, StopToken};
pub use clock::{Clock, ManualClock, SystemClock};
pub use time::{FrameRate, FrameTime, Rational, Rounding, Time, TimeError};
pub use timespec::{TimeSpec, TimeSpecError};
