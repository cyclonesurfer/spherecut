//! Spherecut core: names, exact time and the `--time` grammar, the frame
//! and colour vocabulary, cancellation, and (later) the project model,
//! timeline, keyframes and undo.

#![forbid(unsafe_code)]

pub mod brand;
pub mod cancel;
pub mod time;
pub mod timespec;
pub mod video;

pub use cancel::{StopSource, StopToken};
pub use time::{FrameRate, FrameTime, Rational, Rounding, Time, TimeError};
pub use timespec::{TimeSpec, TimeSpecError};
