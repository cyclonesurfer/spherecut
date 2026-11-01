//! The binary records' payloads (insta360.md §3.3 and §3.4): the
//! thumbnail (2), the IMU's stamps (3) and the exposure (4). M3.2's unit
//! (a). Each takes a payload [`super::read_record`] read and reads
//! nothing itself.

#![expect(
    unused_variables,
    reason = "the bodies are todo!() until M3.2's unit (a) writes them"
)]

use crate::CameraError;

/// One frame's exposure, an item of record 4: 16 bytes, `{u64 t_us, f64
/// seconds}`, little-endian, one a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exposure {
    /// When the frame was exposed, on the camera's clock in µs (the X5's
    /// unit, §3.4): the clock of the metadata's first-frame time.
    pub t_us: u64,
    /// The shutter time, in seconds.
    pub seconds: f64,
}

/// Record 4's items, in the order stored. A payload that isn't whole
/// 16-byte items is [`CameraError::BadSize`].
pub fn parse_exposure(bytes: &[u8]) -> Result<Vec<Exposure>, CameraError> {
    todo!()
}

/// A picture in NV12: the luma plane, then the chroma plane with Cb and
/// Cr interleaved at half the size each way, both without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12Image {
    pub width: u32,
    pub height: u32,
    /// `width × height` bytes.
    pub y: Vec<u8>,
    /// `width × height / 2` bytes: Cb, Cr, Cb, Cr, …
    pub uv: Vec<u8>,
}

/// Record 2, the thumbnail: a 40-byte header, then a stitched equirect
/// in NV12 (§3.3). On Pat's files the header's little-endian u32s are 1,
/// the record's size, 1, 0, the width (1280), the height (640) and four
/// zeros. A size the header's width and height don't account for is
/// [`CameraError::BadSize`].
pub fn thumbnail(bytes: &[u8]) -> Result<Nv12Image, CameraError> {
    todo!()
}

/// What record 3, the IMU, holds, as far as M3 reads it: its items and
/// their first and last stamps. M4 parses the samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImuSpan {
    /// The number of items.
    pub samples: u64,
    /// The first item's stamp, on the camera's clock in µs.
    pub first_us: u64,
    /// The last item's stamp.
    pub last_us: u64,
}

/// Record 3's length and stamps, read as the X5's raw items (field 62 =
/// 1): 20 bytes each, `{u64 t_us, u16 × 6}` (§3.4). `Ok(None)` for an
/// empty record; a payload that isn't whole items is
/// [`CameraError::BadSize`].
pub fn imu_span(bytes: &[u8]) -> Result<Option<ImuSpan>, CameraError> {
    todo!()
}
