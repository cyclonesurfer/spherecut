//! Export (M2 plan section 8, DECISIONS D52): a project's sequence
//! rendered on the GPU, encoded and muxed into an MP4.
//!
//! An [`Export`] runs one [`ExportJob`] on three threads joined by
//! bounded channels (section 8.2): decode, holding each source frame for
//! the output frames it covers; render, the clip's keyed view (or the
//! whole sphere) through the YUV pass into an encoder's planes; and
//! encode and mux, into a temporary file beside the output that becomes
//! the output only when it is whole and its checks have passed (D16,
//! section 9). The trim's audio is encoded between the frames on the
//! encode stage's thread. The handle reports progress, cancels, and
//! hands back the [`Report`] or the [`ExportError`].
//!
//! What needs FFmpeg's C API is in spherecut-media; nothing here is
//! unsafe.

#![forbid(unsafe_code)]

mod audio;
pub mod check;
pub mod encoder;
pub mod error;
pub mod job;
pub mod pace;
mod pipeline;
pub mod plan;
pub mod preset;
pub mod warning;

pub use audio::copy_refusal;
pub use check::Checked;
pub use encoder::{EncoderKind, EncoderSetup};
pub use error::{ExportError, Stage};
pub use job::{
    AudioReport, DecodeReport, Decoder, Export, ExportJob, Fallback, Prepared, Progress, Report,
    StageTimes, needed_bytes, prepare,
};
pub use pace::Pace;
pub use plan::FramePlan;
pub use preset::Preset;
pub use warning::Warning;
