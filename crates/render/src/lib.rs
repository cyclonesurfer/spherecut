//! The render core (M1 plan section 4, DECISIONS D31): a wgpu device on
//! Vulkan, and later the planes, the projection and the CPU reference.

#![forbid(unsafe_code)]

pub mod error;
pub mod gpu;

pub use error::RenderError;
pub use gpu::{AdapterChoice, Gpu, GpuCaps};
pub use wgpu;
