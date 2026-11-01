//! Cameras' raw files (M3 plan section 5 and its D62; DECISIONS D61):
//! what a camera's own bytes say about a recording, read by our own code,
//! with no FFmpeg.
//!
//! [`insta360`] reads an `.insv` or `.lrv` file's trailer (its index and
//! records), the metadata record through a hand-written protobuf walker
//! ([`pb`]), and the calibration strings, which it also prints back byte
//! for byte; and it names a recording's files and checks that a master
//! and its proxy belong together.
//!
//! The readers take `impl Read + Seek`, so their callers open the file,
//! an agent's beneath its root (D45) and the app's on a job, and each
//! reader calls [`spherecut_core::may_block`] before it reads: none may
//! run on the UI thread (D34). The parsers take bytes already read. Bad
//! bytes never panic, and nothing is allocated for a size before it is
//! checked against its limit; every error ([`CameraError`]) says which
//! part of the file it is in and at which byte.
//!
//! media depends on this crate (probe, and a project's camera media), and
//! this crate never on media (D61).

#![forbid(unsafe_code)]

mod error;
pub mod insta360;
pub mod pb;

pub use error::{CameraError, Part};
