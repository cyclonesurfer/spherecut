//! The metadata record, record 1: protobuf, read with [`crate::pb::Walker`]
//! and no schema (insta360.md §3.5; M3 plan 5.1). M3.2's unit (b).
//!
//! Only the fields 5.1 lists are read; every other top-level field is
//! skipped and counted. Their numbers and the values quoted are from the
//! bytes of Pat's X5 files unless the format notes say otherwise. Of
//! 131 to 136, 133 and 134 are kept by nothing: the X5 writes no 133 and
//! 0 in 134, whose meaning is unknown, so both are counted as unknown.

use spherecut_core::project::file::fnv1a64;

use super::calib::Generation;
use super::le64;
use super::limits::{MAX_METADATA, MAX_STRING};
use crate::pb::{Field, Value, Walker};
use crate::{CameraError, Part};

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
    /// The X5 writes an f32 widened to the double, 21.244001388549805.
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

    /// The calibration's hash, as a project's snapshot of it records it
    /// (M3 plan 11.1): 16 lowercase hex digits of core's FNV-1a 64 over
    /// the working strings in generation order, V1, V2, V3 and V6, each
    /// ended by a NUL byte, so that a generation the file lacks is its
    /// NUL alone. Probe prints it (5.4).
    pub fn hash(&self) -> String {
        let mut bytes = Vec::new();
        for g in Generation::ALL {
            if let Some(s) = &self.get(g).working {
                bytes.extend(s.as_bytes());
            }
            bytes.push(0);
        }
        format!("{:016x}", fnv1a64(&bytes))
    }

    /// Generation `g`'s strings, to set.
    fn get_mut(&mut self, g: Generation) -> &mut CalibStrings {
        match g {
            Generation::V1 => &mut self.v1,
            Generation::V2 => &mut self.v2,
            Generation::V3 => &mut self.v3,
            Generation::V6 => &mut self.v6,
        }
    }
}

/// Reads record 1's payload. Fields are found by number whatever their
/// order. A number or a string read twice keeps its last value, as
/// protobuf does; a message read twice (26, 27, 44, 98 or 145) is
/// replaced whole by its last copy, where protobuf would merge the two.
///
/// A payload over [`MAX_METADATA`] or a string over [`MAX_STRING`] is
/// [`CameraError::TooLarge`]; a bad varint, a group or a field cut short
/// is an error at its byte, and a string field that isn't UTF-8 is
/// [`CameraError::NotUtf8`]. Unknown fields are skipped by their wire
/// type and counted in [`Metadata::unknown`].
///
/// A field read here, or one inside its message, that comes in a wire
/// type other than its own is [`CameraError::WireType`] at its key, and a
/// varint past 32 bits where the field holds a u32 (the window and the
/// video size) is [`CameraError::BadVarint`] at the value. Errors inside
/// a field are in its [`Part::Field`]; those of the walk between fields,
/// and the payload's size, in `Part::Record(1)`. The payload's
/// `TooLarge` is at the first byte past the limit, and a string's at the
/// key of its field. The fields inside a message read here that it
/// doesn't read are skipped, and not counted.
pub fn parse_metadata(bytes: &[u8]) -> Result<Metadata, CameraError> {
    if bytes.len() > MAX_METADATA {
        return Err(CameraError::TooLarge {
            part: Part::Record(1),
            size: bytes.len() as u64,
            limit: MAX_METADATA as u64,
            offset: MAX_METADATA as u64,
        });
    }
    let mut m = Metadata::default();
    for f in Walker::new(bytes, Part::Record(1), 0) {
        let f = f?;
        let n = f.number;
        match n {
            1 => m.serial = Some(string(&f, n)?),
            2 => m.model = Some(string(&f, n)?),
            3 => m.firmware = Some(string(&f, n)?),
            24 => m.first_frame_us = Some(varint(&f, n)? as i64),
            25 => m.rolling_shutter_ms = Some(double(&f, n)?),
            26 => m.file_group = Some(file_group(&f)?),
            27 => m.window = Some(window(&f)?),
            44 => m.video_size = Some(video_size(&f)?),
            79 => m.layout = Some(varint(&f, n)?),
            80 => m.track_order = Some(varint(&f, n)?),
            93 => m.duration_ms = Some(varint(&f, n)?),
            98 => m.frames = Some(frame_span(&f)?),
            131 => m.stream_type = Some(varint(&f, n)?),
            132 => m.codec = Some(varint(&f, n)?),
            135 => m.bitrate_mbps = Some(varint(&f, n)?),
            136 => m.offset_version = Some(varint(&f, n)?),
            145 => m.lens_profiles = Some(lens_profiles(&f)?),
            _ => match Generation::ALL
                .into_iter()
                .find(|g| g.fields().contains(&n))
            {
                Some(g) => {
                    let s = Some(string(&f, n)?);
                    let c = m.calib.get_mut(g);
                    if n == g.fields()[0] {
                        c.working = s;
                    } else {
                        c.original = s;
                    }
                }
                None => m.unknown = m.unknown.saturating_add(1),
            },
        }
    }
    Ok(m)
}

/// [`CameraError::WireType`] for field `f`, inside top-level field `top`,
/// which came in a wire type other than its own.
fn wire_type(f: &Field, top: u32) -> CameraError {
    let wire = match f.value {
        Value::Varint(_) => 0,
        Value::I64(_) => 1,
        Value::Len(_) => 2,
        Value::I32(_) => 5,
    };
    CameraError::WireType {
        part: Part::Field(top),
        wire,
        offset: f.offset,
    }
}

/// `f`'s value as a varint (an integer, a bool or an enum).
fn varint(f: &Field, top: u32) -> Result<u64, CameraError> {
    match f.value {
        Value::Varint(v) => Ok(v),
        _ => Err(wire_type(f, top)),
    }
}

/// `f`'s value as a varint that fits a u32.
fn small(f: &Field, top: u32) -> Result<u32, CameraError> {
    u32::try_from(varint(f, top)?).map_err(|_| CameraError::BadVarint {
        part: Part::Field(top),
        offset: f.value_offset,
    })
}

/// `f`'s value as a double.
fn double(f: &Field, top: u32) -> Result<f64, CameraError> {
    match f.value {
        Value::I64(v) => Ok(f64::from_bits(v)),
        _ => Err(wire_type(f, top)),
    }
}

/// `f`'s bytes: a string, bytes or a message.
fn len<'a>(f: &Field<'a>, top: u32) -> Result<&'a [u8], CameraError> {
    match f.value {
        Value::Len(b) => Ok(b),
        _ => Err(wire_type(f, top)),
    }
}

/// `f`'s value as a string of at most [`MAX_STRING`] bytes.
fn string(f: &Field, top: u32) -> Result<String, CameraError> {
    let b = len(f, top)?;
    if b.len() > MAX_STRING {
        return Err(CameraError::TooLarge {
            part: Part::Field(top),
            size: b.len() as u64,
            limit: MAX_STRING as u64,
            offset: f.offset,
        });
    }
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.to_owned()),
        Err(e) => Err(CameraError::NotUtf8 {
            part: Part::Field(top),
            offset: f.value_offset + e.valid_up_to() as u64,
        }),
    }
}

/// A walker over top-level field `f`'s message.
fn message<'a>(f: &Field<'a>) -> Result<Walker<'a>, CameraError> {
    let part = Part::Field(f.number);
    Ok(Walker::new(len(f, f.number)?, part, f.value_offset))
}

/// Field 26.
fn file_group(f: &Field) -> Result<FileGroup, CameraError> {
    let mut group = FileGroup::default();
    for g in message(f)? {
        let g = g?;
        match g.number {
            1 => group.kind = varint(&g, f.number)?,
            2 => group.index = varint(&g, f.number)?,
            3 => group.identify = Some(string(&g, f.number)?),
            4 => group.total = varint(&g, f.number)?,
            _ => {}
        }
    }
    Ok(group)
}

/// Field 27.
fn window(f: &Field) -> Result<Window, CameraError> {
    let mut w = Window::default();
    for g in message(f)? {
        let g = g?;
        let slot = match g.number {
            1 => &mut w.src[0],
            2 => &mut w.src[1],
            3 => &mut w.dst[0],
            4 => &mut w.dst[1],
            5 => &mut w.offset[0],
            6 => &mut w.offset[1],
            _ => continue,
        };
        *slot = small(&g, f.number)?;
    }
    Ok(w)
}

/// Field 44, `{1 width, 2 height}`.
fn video_size(f: &Field) -> Result<[u32; 2], CameraError> {
    let mut size = [0; 2];
    for g in message(f)? {
        let g = g?;
        let slot = match g.number {
            1 => &mut size[0],
            2 => &mut size[1],
            _ => continue,
        };
        *slot = small(&g, f.number)?;
    }
    Ok(size)
}

/// Field 98.
fn frame_span(f: &Field) -> Result<FrameSpan, CameraError> {
    let mut span = FrameSpan::default();
    for g in message(f)? {
        let g = g?;
        let slot = match g.number {
            1 => &mut span.first_us,
            3 => &mut span.first_unix_ms,
            4 => &mut span.last_unix_ms,
            5 => &mut span.frames,
            _ => continue,
        };
        *slot = varint(&g, f.number)?;
    }
    Ok(span)
}

/// Field 145: its items 1 (a name and a polynomial's coefficients) and 2
/// (a name and a field of view).
fn lens_profiles(f: &Field) -> Result<LensProfiles, CameraError> {
    let top = f.number;
    let mut profiles = LensProfiles::default();
    for item in message(f)? {
        let item = item?;
        if !matches!(item.number, 1 | 2) {
            continue;
        }
        let (mut name, mut values) = (String::new(), Vec::new());
        for g in Walker::new(len(&item, top)?, Part::Field(top), item.value_offset) {
            let g = g?;
            match (g.number, g.value) {
                (1, _) => name = string(&g, top)?,
                // A repeated double, one a field or packed in one.
                (2, Value::Len(packed)) => {
                    if packed.len() % 8 != 0 {
                        return Err(CameraError::Truncated {
                            part: Part::Field(top),
                            offset: g.value_offset + packed.len() as u64,
                        });
                    }
                    let n = packed.len() / 8;
                    values.extend((0..n).map(|i| f64::from_bits(le64(packed, 8 * i))));
                }
                (2, _) => values.push(double(&g, top)?),
                _ => {}
            }
        }
        if item.number == 1 {
            profiles.polynomials.push((name, values));
        } else {
            // The last value, as for any field read twice.
            profiles
                .fovs
                .push((name, values.last().copied().unwrap_or(0.0)));
        }
    }
    Ok(profiles)
}
