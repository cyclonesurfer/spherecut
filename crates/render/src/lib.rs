//! The render core (M1 plan section 4, DECISIONS D31): a wgpu device on
//! Vulkan, decoded planes uploaded into canonical textures with area mip
//! chains, the projection shader that turns an equirect frame into a
//! view, the encode pass, headless stills, and the CPU reference the
//! tests hold the GPU to.
//!
//! Data flows one way: [`VideoPlanes`](spherecut_core::video::VideoPlanes)
//! → [`Uploader`] → a [`GpuFrame`] slot → [`Projector`] → a
//! [`LinearTarget`] → [`Encoder`] → the screen, or a readback
//! ([`render_still`]), or → [`YuvConverter`] → an encoder's planes (M2). A decoder's picture can arrive as DMA-BUF planes
//! instead ([`dmabuf`]), which the uploader copies from on the GPU, or as
//! Vulkan images on the renderer's own device ([`vkshare`]), copied into
//! a buffer the uploader reads.

// Only `dmabuf`, the DMA-BUF import, and `vkshare`, the device shared
// with FFmpeg's Vulkan Video, may use unsafe (M1 plan section 2.4,
// DECISIONS D25).
#![deny(unsafe_code)]

pub mod blur;
pub mod color;
pub mod compare;
#[allow(unsafe_code)]
pub mod dmabuf;
pub mod encode;
pub mod error;
pub mod frame;
pub mod gpu;
pub mod layouts;
pub mod output;
pub mod plane;
pub mod project;
pub mod readback;
pub mod reference;
pub mod still;
pub mod timers;
pub mod upload;
#[allow(unsafe_code)]
pub mod vkshare;
pub mod yuv;

pub use blur::Blur;
pub use color::{Placement, SourceUniform, Stereo};
pub use encode::{Encoder, PlaceUniform};
pub use error::RenderError;
pub use frame::GpuFrame;
pub use gpu::{AdapterChoice, Gpu, GpuCaps, POLL_EVERY};
pub use output::FrameViews;
pub use plane::{LayoutKind, PlaneFormat, PlaneShape, SlotShape};
pub use project::{
    DebugMode, Filter, Framing, LinearTarget, OutKind, Projector, ViewParams, ViewUniform,
};
pub use still::{Depth, Image, Pixels, StillOptions, StillRenderer, YuvImage, render_still};
pub use timers::{GpuTimers, TimerSpan};
pub use upload::{IngestUniform, UploadPath, UploadStats, Uploader};
pub use wgpu;
pub use yuv::{YuvConverter, YuvLayout, YuvShape};
