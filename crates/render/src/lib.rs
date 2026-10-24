//! The render core (M1 plan section 4, DECISIONS D31): a wgpu device on
//! Vulkan, decoded planes uploaded into canonical textures with area mip
//! chains, the projection shader that turns an equirect frame into a
//! view, the encode pass, headless stills, and the CPU reference the
//! tests hold the GPU to.
//!
//! Data flows one way: [`VideoPlanes`](spherecut_core::video::VideoPlanes)
//! → [`Uploader`] → a [`GpuFrame`] slot → [`Projector`] → a
//! [`LinearTarget`] → [`Encoder`] → the screen, or a readback
//! ([`render_still`]).

#![forbid(unsafe_code)]

pub mod color;
pub mod encode;
pub mod error;
pub mod frame;
pub mod gpu;
pub mod layouts;
pub mod plane;
pub mod project;
pub mod readback;
pub mod reference;
pub mod still;
pub mod timers;
pub mod upload;

pub use color::{Placement, SourceUniform, Stereo};
pub use encode::{Encoder, PlaceUniform};
pub use error::RenderError;
pub use frame::GpuFrame;
pub use gpu::{AdapterChoice, Gpu, GpuCaps};
pub use plane::{LayoutKind, PlaneFormat, PlaneShape, SlotShape};
pub use project::{DebugMode, Filter, LinearTarget, Projector, ViewParams, ViewUniform};
pub use still::{Depth, Image, Pixels, StillOptions, StillRenderer, render_still};
pub use timers::{GpuTimers, TimerSpan};
pub use upload::{IngestUniform, UploadPath, UploadStats, Uploader};
pub use wgpu;
