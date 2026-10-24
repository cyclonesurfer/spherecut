//! Spherecut core: names, exact time, and (later) the project model,
//! timeline, keyframes and undo.

pub mod brand;
pub mod time;

pub use time::{FrameRate, FrameTime, Rational, Rounding, Time, TimeError};
