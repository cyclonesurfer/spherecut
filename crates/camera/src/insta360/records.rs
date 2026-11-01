//! The binary records' payloads (insta360.md §3.3 and §3.4): the
//! thumbnail (2), the IMU's stamps (3) and the exposure (4). M3.2's unit
//! (a). Each takes a payload [`super::read_record`] read and reads
//! nothing itself.

use super::{le32, le64};
use crate::{CameraError, Part};

/// An exposure item: `{u64 t_us, f64 seconds}`.
const EXPOSURE_ITEM: usize = 16;
/// The thumbnail's header, before its planes.
const THUMBNAIL_HEADER: usize = 40;
/// Where in the header the width is, then the height.
const WIDTH_AT: usize = 16;
const HEIGHT_AT: usize = 20;
/// A raw IMU item: `{u64 t_us, u16 × 6}`.
const IMU_ITEM: usize = 20;

/// [`CameraError::BadSize`] for record `id`, whose `len` bytes aren't
/// whole `item`-byte items, at the first byte of the part item.
fn not_whole(id: u8, len: usize, item: usize) -> Option<CameraError> {
    let part = len % item;
    (part != 0).then(|| CameraError::BadSize {
        id,
        size: len as u64,
        offset: (len - part) as u64,
    })
}

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
    if let Some(e) = not_whole(4, bytes.len(), EXPOSURE_ITEM) {
        return Err(e);
    }
    Ok(bytes
        .chunks_exact(EXPOSURE_ITEM)
        .map(|item| Exposure {
            t_us: le64(item, 0),
            seconds: f64::from_bits(le64(item, 8)),
        })
        .collect())
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
/// [`CameraError::BadSize`] at the width, as is an odd width or height,
/// which NV12 can't halve; a payload shorter than the header is
/// [`CameraError::Truncated`] at its end.
pub fn thumbnail(bytes: &[u8]) -> Result<Nv12Image, CameraError> {
    let Some((header, planes)) = bytes.split_first_chunk::<THUMBNAIL_HEADER>() else {
        return Err(CameraError::Truncated {
            part: Part::Record(2),
            offset: bytes.len() as u64,
        });
    };
    let (width, height) = (le32(header, WIDTH_AT), le32(header, HEIGHT_AT));
    let bad = || CameraError::BadSize {
        id: 2,
        size: bytes.len() as u64,
        offset: WIDTH_AT as u64,
    };
    if width % 2 != 0 || height % 2 != 0 {
        return Err(bad());
    }
    // Both planes, in u64 so that no width and height overflow.
    let luma = u64::from(width) * u64::from(height);
    if luma.checked_add(luma / 2) != Some(planes.len() as u64) {
        return Err(bad());
    }
    let (y, uv) = planes.split_at_checked(luma as usize).ok_or_else(bad)?;
    Ok(Nv12Image {
        width,
        height,
        y: y.to_vec(),
        uv: uv.to_vec(),
    })
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
    if let Some(e) = not_whole(3, bytes.len(), IMU_ITEM) {
        return Err(e);
    }
    let Some(last) = bytes.len().checked_sub(IMU_ITEM) else {
        return Ok(None);
    };
    Ok(Some(ImuSpan {
        samples: (bytes.len() / IMU_ITEM) as u64,
        first_us: le64(bytes, 0),
        last_us: le64(bytes, last),
    }))
}
