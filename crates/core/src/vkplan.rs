//! What a Vulkan device must have to be shared between wgpu and FFmpeg's
//! Vulkan Video (slice 5b's path B, DECISIONS D25): its queues and its
//! extensions, from what the device lists. Pure, so that render, which
//! opens the shared device, and media's doctor, which says whether a
//! device could be, plan it the same way.
//!
//! Flags are Vulkan's own bits (`VkQueueFlags`,
//! `VkVideoCodecOperationFlagsKHR`), as the device reports them.

use std::cmp::Reverse;
use std::ffi::CStr;

use crate::video::VkQueueGrant;

/// `VK_QUEUE_GRAPHICS_BIT`.
pub const QUEUE_GRAPHICS: u32 = 0x1;
/// `VK_QUEUE_COMPUTE_BIT`.
pub const QUEUE_COMPUTE: u32 = 0x2;
/// `VK_QUEUE_TRANSFER_BIT`.
pub const QUEUE_TRANSFER: u32 = 0x4;
/// `VK_QUEUE_VIDEO_DECODE_BIT_KHR`.
pub const QUEUE_VIDEO_DECODE: u32 = 0x20;

/// `VK_VIDEO_CODEC_OPERATION_DECODE_H264_BIT_KHR`.
pub const DECODE_H264: u32 = 0x1;
/// `VK_VIDEO_CODEC_OPERATION_DECODE_H265_BIT_KHR`.
pub const DECODE_H265: u32 = 0x2;
/// `VK_VIDEO_CODEC_OPERATION_DECODE_AV1_BIT_KHR`.
pub const DECODE_AV1: u32 = 0x4;

/// The codecs FFmpeg's Vulkan Video decodes for Spherecut.
const CODECS: u32 = DECODE_H264 | DECODE_H265;

/// `VK_API_VERSION_1_3`: FFmpeg calls 1.3's entry points.
pub const API_VERSION_1_3: u32 = (1 << 22) | (3 << 12);

/// `VK_KHR_video_queue`, which a device needs for its queue families to
/// say what they decode.
pub const VIDEO_QUEUE: &CStr = c"VK_KHR_video_queue";
const VIDEO_DECODE_QUEUE: &CStr = c"VK_KHR_video_decode_queue";
const VIDEO_DECODE_H264: &CStr = c"VK_KHR_video_decode_h264";
const VIDEO_DECODE_H265: &CStr = c"VK_KHR_video_decode_h265";
const VIDEO_DECODE_AV1: &CStr = c"VK_KHR_video_decode_av1";
const VIDEO_MAINTENANCE1: &CStr = c"VK_KHR_video_maintenance1";

/// One queue family as the device lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Family {
    /// `VkQueueFlags`.
    pub flags: u32,
    pub count: u32,
    /// Its video codec operations; 0 for a family without video.
    pub video_ops: u32,
}

/// The queues a shared device is made with besides wgpu's own (family 0,
/// queue 0, which no one else submits to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuePlan {
    /// A compute family other than wgpu's: its queue 0 is FFmpeg's (frame
    /// preparation, uploads and downloads), its queue 1 the copier's.
    pub compute: u32,
    /// A video-decode family, whose queue 0 is FFmpeg's. It may be the
    /// compute family.
    pub decode: u32,
    /// The codecs it decodes of those Spherecut asks for, AV1 included.
    pub decode_ops: u32,
}

impl QueuePlan {
    /// The queue the copier submits to: family and index.
    pub fn copy_queue(&self) -> (u32, u32) {
        (self.compute, 1)
    }

    /// Each family besides wgpu's and how many of its queues the device
    /// is made with.
    pub fn queue_counts(&self) -> Vec<(u32, u32)> {
        if self.decode == self.compute {
            vec![(self.compute, 2)]
        } else {
            vec![(self.compute, 2), (self.decode, 1)]
        }
    }

    /// The queues FFmpeg is given: queue 0 of each family, for what each
    /// is for. The copier's queue shares the compute family, so FFmpeg's
    /// frames, made `CONCURRENT` over these families, are its to read.
    pub fn grants(&self) -> Vec<VkQueueGrant> {
        let compute = QUEUE_COMPUTE | QUEUE_TRANSFER;
        if self.decode == self.compute {
            return vec![VkQueueGrant {
                family: self.compute,
                count: 1,
                flags: compute | QUEUE_VIDEO_DECODE,
                video_ops: self.decode_ops,
            }];
        }
        vec![
            VkQueueGrant {
                family: self.compute,
                count: 1,
                flags: compute,
                video_ops: 0,
            },
            VkQueueGrant {
                family: self.decode,
                count: 1,
                flags: QUEUE_VIDEO_DECODE,
                video_ops: self.decode_ops,
            },
        ]
    }
}

/// The queues a shared device needs from `families` (the device's, in
/// its order), or why it can't have them. Family 0 is wgpu's and is
/// never planned. The decode family is the one with H.265 first, then
/// the most codecs; the compute family one without graphics first.
pub fn plan_queues(families: &[Family]) -> Result<QueuePlan, String> {
    let (decode, d) = families
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, f)| {
            f.flags & QUEUE_VIDEO_DECODE != 0 && f.count >= 1 && f.video_ops & CODECS != 0
        })
        .max_by_key(|(i, f)| {
            (
                f.video_ops & DECODE_H265 != 0,
                f.video_ops.count_ones(),
                Reverse(*i),
            )
        })
        .ok_or_else(|| {
            if families.iter().any(|f| f.flags & QUEUE_VIDEO_DECODE != 0) {
                "no video-decode queue family besides wgpu's decodes H.264 or H.265".to_owned()
            } else {
                "the device has no video-decode queue family".to_owned()
            }
        })?;
    let compute = families
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, f)| f.flags & QUEUE_COMPUTE != 0 && f.count >= 2)
        .min_by_key(|(i, f)| (f.flags & QUEUE_GRAPHICS != 0, *i))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            "no compute queue family besides wgpu's has two queues (one for FFmpeg, one for \
             the copies)"
                .to_owned()
        })?;
    Ok(QueuePlan {
        compute: compute as u32,
        decode: decode as u32,
        decode_ops: d.video_ops & (CODECS | DECODE_AV1),
    })
}

/// The device extensions a shared device adds to wgpu's, from those the
/// device has (`available`), or why it can't be one. `api_version` is
/// what the device runs at, packed as `VK_MAKE_API_VERSION`.
pub fn plan_extensions(
    available: &[&CStr],
    api_version: u32,
) -> Result<Vec<&'static CStr>, String> {
    if api_version < API_VERSION_1_3 {
        return Err(format!(
            "the device runs Vulkan {}.{}, and FFmpeg's Vulkan Video needs 1.3",
            (api_version >> 22) & 0x7f,
            (api_version >> 12) & 0x3ff
        ));
    }
    let has = |name: &CStr| available.contains(&name);
    let mut add = Vec::new();
    for name in [VIDEO_QUEUE, VIDEO_DECODE_QUEUE] {
        if !has(name) {
            return Err(format!("the device lacks {}", name.to_string_lossy()));
        }
        add.push(name);
    }
    let codecs = [VIDEO_DECODE_H264, VIDEO_DECODE_H265];
    if !codecs.iter().any(|c| has(c)) {
        return Err("the device decodes neither H.264 nor H.265 through Vulkan Video".into());
    }
    let optional = [VIDEO_DECODE_AV1, VIDEO_MAINTENANCE1];
    add.extend(codecs.into_iter().chain(optional).filter(|c| has(c)));
    Ok(add)
}

/// Whether a device with these extensions, API version and queue
/// families could be shared, or why not: the extensions first, as
/// opening it would find them.
pub fn shareable(available: &[&CStr], api_version: u32, families: &[Family]) -> Result<(), String> {
    plan_extensions(available, api_version)?;
    plan_queues(families)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_shareable_with_both_its_extensions_and_its_queues() {
        let exts = [VIDEO_QUEUE, VIDEO_DECODE_QUEUE, VIDEO_DECODE_H265];
        let radv = [
            Family {
                flags: QUEUE_GRAPHICS | QUEUE_COMPUTE | QUEUE_TRANSFER,
                count: 1,
                video_ops: 0,
            },
            Family {
                flags: QUEUE_COMPUTE | QUEUE_TRANSFER,
                count: 4,
                video_ops: 0,
            },
            Family {
                flags: QUEUE_VIDEO_DECODE,
                count: 1,
                video_ops: CODECS | DECODE_AV1,
            },
        ];
        assert_eq!(shareable(&exts, API_VERSION_1_3, &radv), Ok(()));
        // The extensions are judged first.
        assert_eq!(
            shareable(&exts[1..], API_VERSION_1_3, &radv[..1]).unwrap_err(),
            "the device lacks VK_KHR_video_queue"
        );
        assert_eq!(
            shareable(&exts, API_VERSION_1_3, &radv[..2]).unwrap_err(),
            "the device has no video-decode queue family"
        );
        assert_eq!(
            shareable(&exts, (1 << 22) | (2 << 12) | 290, &radv).unwrap_err(),
            "the device runs Vulkan 1.2, and FFmpeg's Vulkan Video needs 1.3"
        );
    }
}
