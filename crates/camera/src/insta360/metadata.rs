//! The metadata record, record 1: protobuf, read with [`crate::pb::Walker`]
//! and no schema (insta360.md §3.5; M3 plan 5.1). M3.2's unit (b).
//!
//! Only the fields 5.1 lists are read; every other top-level field is
//! skipped and counted. Their numbers and the values quoted are from the
//! bytes of Pat's X5 files unless the format notes say otherwise.

#![expect(
    unused_variables,
    reason = "the bodies are todo!() until M3.2's unit (b) writes them"
)]

use super::calib::Generation;
use crate::CameraError;

/// What record 1 says about the camera and the recording.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// Field 1, the camera's serial number.
    pub serial: Option<String>,
    /// Field 2, the model: `"Insta360 X5"`.
    pub model: Option<String>,
    /// Field 3, the firmware: `"v1.1.22_build1"`.
    pub firmware: Option<String>,
    /// Field 24: the camera's clock at the first video frame, in µs on
    /// the X5, as the IMU's and the exposure's stamps are (§3.4).
    pub first_frame_us: Option<i64>,
    /// Field 25: the rolling shutter's readout, in ms (21.244 at 8K30).
    pub rolling_shutter_ms: Option<f64>,
    /// Field 26, which files make the recording.
    pub file_group: Option<FileGroup>,
    /// Field 27, the part of each lens's square on the calibration canvas
    /// that the tracks show.
    pub window: Option<Window>,
    /// Field 44: the size of this file's own video, width and height. A
    /// master gives one track's (3840² at 8K), an LRV its frame's
    /// (1664 × 832), so on an LRV it is the LRV's size.
    pub video_size: Option<[u32; 2]>,
    /// Field 79: 1 for split files, 2 for both lenses as tracks of one
    /// file (§2.6). 2 on the X5.
    pub layout: Option<u64>,
    /// Field 80, the tracks' order (§2.6). 1 on the X5.
    pub track_order: Option<u64>,
    /// Field 93: the recording's length in ms (3837 for clip 1's 3.8 s).
    pub duration_ms: Option<u64>,
    /// Field 98, the first and last frames' times and the frame count.
    pub frames: Option<FrameSpan>,
    /// Field 131, the streams' layout (§2.6): 4 on Pat's masters, 1 on
    /// their LRVs.
    pub stream_type: Option<u64>,
    /// Field 132, the codec: 2 (H.265) on Pat's masters, 1 (H.264) on
    /// their LRVs.
    pub codec: Option<u64>,
    /// Field 135, the bit rate in Mb/s: 120 at 8K30, 150 at 5.7K60, 7 on
    /// an LRV.
    pub bitrate_mbps: Option<u64>,
    /// Field 136, the calibration generation the camera says it uses: 1
    /// V1, 2 V2, 3 V3, 4 V6 (4 on the X5).
    pub offset_version: Option<u64>,
    /// Field 145, the lens profiles per accessory.
    pub lens_profiles: Option<LensProfiles>,
    /// The calibration strings: fields 5 and 17, 53 and 55, 54 and 56,
    /// 111 and 112.
    pub calib: Calibrations,
    /// The top-level fields not read here, counted (5.1).
    pub unknown: u32,
}

/// Field 26, `{1 type, 2 index, 3 identify, 4 total}` (§2.4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileGroup {
    /// The recording's kind: 0 on Pat's files, 6 on the 5.7K60 take shot
    /// in the camera's HDR video mode.
    pub kind: u64,
    /// 0 on a master, 1 on its LRV.
    pub index: u64,
    /// A path on the camera's card: `/DCIM/Camera01/VID_…_00_NNN.insv`
    /// on Pat's masters.
    pub identify: Option<String>,
    /// 0 on the X5.
    pub total: u64,
}

/// Field 27, `{1 src_w, 2 src_h, 3 dst_w, 4 dst_h, 5 x, 6 y}`: on the X5
/// a 5312² window of each lens's 5376² square, at 0, 0 (§4.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Window {
    pub src: [u32; 2],
    pub dst: [u32; 2],
    pub offset: [u32; 2],
}

/// Field 98's items 1, 3, 4 and 5 (§3.5). Item 2's meaning is in conflict
/// (the format notes), so it isn't kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameSpan {
    /// Item 1: the first frame on the camera's clock, in µs; field 24's
    /// value on Pat's files.
    pub first_us: u64,
    /// Item 3: the first frame, in Unix ms.
    pub first_unix_ms: u64,
    /// Item 4: the last frame, in Unix ms.
    pub last_unix_ms: u64,
    /// Item 5: the frame count (115 for clip 1).
    pub frames: u64,
}

/// Field 145: per accessory, by name (`bare`, `ProtectorA`, …), a
/// polynomial in degrees and a field of view (§3.5, §4.6).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LensProfiles {
    /// Its repeated item 1, `{1 name, 2 coefficient…}`: bare is 0,
    /// 0.03159, −8.415e−5, 2.201e−6, −1.284e−8, 0.
    pub polynomials: Vec<(String, Vec<f64>)>,
    /// Its repeated item 2, `{1 name, 2 degrees}`: bare 200, the
    /// protectors 195, the dive cases 190.
    pub fovs: Vec<(String, f64)>,
}

/// One generation's calibration strings as stored: the working string
/// and the original copy, which are equal on every file seen (§4.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalibStrings {
    /// Field 5, 53, 54 or 111.
    pub working: Option<String>,
    /// Field 17, 55, 56 or 112.
    pub original: Option<String>,
}

/// The four generations' strings, each as [`calib::parse`] takes it.
///
/// [`calib::parse`]: super::calib::parse
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Calibrations {
    pub v1: CalibStrings,
    pub v2: CalibStrings,
    pub v3: CalibStrings,
    pub v6: CalibStrings,
}

impl Calibrations {
    /// Generation `g`'s strings.
    pub fn get(&self, g: Generation) -> &CalibStrings {
        match g {
            Generation::V1 => &self.v1,
            Generation::V2 => &self.v2,
            Generation::V3 => &self.v3,
            Generation::V6 => &self.v6,
        }
    }
}

/// Reads record 1's payload. Fields are found by number whatever their
/// order; a field read twice keeps its last value, as protobuf does.
///
/// A payload over [`MAX_METADATA`] or a string over [`MAX_STRING`] is
/// [`CameraError::TooLarge`]; a bad varint, a group or a field cut short
/// is an error at its byte, and a string field that isn't UTF-8 is
/// [`CameraError::NotUtf8`]. Unknown fields are skipped by their wire
/// type and counted in [`Metadata::unknown`].
///
/// [`MAX_METADATA`]: super::limits::MAX_METADATA
/// [`MAX_STRING`]: super::limits::MAX_STRING
pub fn parse_metadata(bytes: &[u8]) -> Result<Metadata, CameraError> {
    todo!()
}
