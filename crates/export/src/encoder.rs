//! The software encoders and how they are set (M2 plan section 8.3,
//! amendment 2 item 24): CRF at a preset with the preset's bitrate as a
//! VBV cap, a keyframe every second in closed GOPs, BT.709 limited range
//! with left chroma siting (what the YUV pass writes).

use spherecut_core::project::{Codec, Quality};
use spherecut_core::time::{FrameRate, Rounding};
use spherecut_media::encode::{ColorTags, PixelLayout};
use spherecut_render::yuv::YuvLayout;

use crate::preset::Preset;

/// FFmpeg's `AVCHROMA_LOC_LEFT`: the YUV pass sites chroma left.
pub const CHROMA_LEFT: i32 = 1;

/// How the export encodes: which encoder, its options, and the planes
/// the YUV pass writes for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderSetup {
    /// FFmpeg's name for it, such as `libx265`.
    pub name: &'static str,
    /// What the YUV pass writes and the encoder takes: I420, or I010
    /// for 10-bit.
    pub yuv: YuvLayout,
    /// The same as the writer's layout.
    pub layout: PixelLayout,
    /// The preset's bitrate, in bits a second.
    pub bitrate: u64,
    /// The VBV cap: 1.5 × the bitrate.
    pub maxrate: u64,
    /// The VBV buffer: two seconds of `maxrate`, without which x264 and
    /// x265 ignore the cap (amendment 2 item 24).
    pub bufsize: u64,
    /// Frames from one keyframe to the next: the most that fit in a
    /// second.
    pub keyint: u32,
    /// The encoder's options, as FFmpeg takes them.
    pub options: Vec<(String, String)>,
    pub color: ColorTags,
}

impl EncoderSetup {
    /// The software encoder for `codec` (libx264, libx265 or
    /// libsvtav1) at `quality`, for `preset` at `rate`. `ten_bit` asks
    /// for 10-bit, which H.264 never gets (section 8.3: a 10-bit source
    /// exports 8-bit H.264).
    pub fn software(
        codec: Codec,
        quality: Quality,
        preset: Preset,
        rate: FrameRate,
        ten_bit: bool,
    ) -> EncoderSetup {
        let pick = |high: u32, medium: u32, small: u32| match quality {
            Quality::High => high,
            Quality::Medium => medium,
            Quality::Small => small,
        };
        let ten_bit = ten_bit && codec != Codec::H264;
        let bitrate = preset.bitrate(quality, codec, rate);
        let maxrate = bitrate * 3 / 2;
        let bufsize = maxrate * 2;
        // At least one frame, at most a second's worth.
        let keyint = rate
            .as_rational()
            .round(Rounding::Floor)
            .clamp(1, i64::from(u32::MAX)) as u32;
        let (name, crf, speed, params) = match codec {
            Codec::H264 => ("libx264", pick(18, 21, 24), "medium", "x264-params"),
            Codec::Hevc => ("libx265", pick(20, 23, 26), "fast", "x265-params"),
            Codec::Av1 => ("libsvtav1", pick(28, 32, 36), "8", "svtav1-params"),
        };
        let mut options: Vec<(String, String)> = vec![
            ("crf".into(), crf.to_string()),
            ("preset".into(), speed.into()),
            ("maxrate".into(), maxrate.to_string()),
            ("bufsize".into(), bufsize.to_string()),
            ("g".into(), keyint.to_string()),
        ];
        // Closed GOPs: x264's default, not x265's; SVT-AV1's key frames
        // (`irefresh-type` 2) rather than its forward ones.
        let gop = match codec {
            Codec::H264 => "open-gop=0",
            Codec::Hevc => "open-gop=0:log-level=error",
            Codec::Av1 => "irefresh-type=2",
        };
        options.push((params.into(), gop.into()));
        let (yuv, layout) = if ten_bit {
            (YuvLayout::I010, PixelLayout::Yuv420p10)
        } else {
            (YuvLayout::I420, PixelLayout::Yuv420p)
        };
        EncoderSetup {
            name,
            yuv,
            layout,
            bitrate,
            maxrate,
            bufsize,
            keyint,
            options,
            color: ColorTags::BT709_LIMITED.with_chroma_location(CHROMA_LEFT),
        }
    }

    /// The option `key`'s value, if it is set.
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::Aspect;

    use super::*;

    const WIDE_1080: Preset = Preset::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };

    #[test]
    fn each_codec_gets_its_encoder_crf_and_speed_by_quality() {
        let rate = FrameRate::FPS_30;
        let cases = [
            (Codec::H264, "libx264", [18, 21, 24], "medium"),
            (Codec::Hevc, "libx265", [20, 23, 26], "fast"),
            (Codec::Av1, "libsvtav1", [28, 32, 36], "8"),
        ];
        for (codec, name, crfs, speed) in cases {
            for (q, crf) in [Quality::High, Quality::Medium, Quality::Small]
                .into_iter()
                .zip(crfs)
            {
                let s = EncoderSetup::software(codec, q, WIDE_1080, rate, false);
                assert_eq!(s.name, name);
                assert_eq!(
                    s.option("crf"),
                    Some(crf.to_string().as_str()),
                    "{name} {q:?}"
                );
                assert_eq!(s.option("preset"), Some(speed), "{name}");
            }
        }
    }

    #[test]
    fn the_cap_is_one_and_a_half_times_the_bitrate_over_two_seconds() {
        let s = EncoderSetup::software(
            Codec::H264,
            Quality::High,
            WIDE_1080,
            FrameRate::FPS_30,
            false,
        );
        assert_eq!(
            (s.bitrate, s.maxrate, s.bufsize),
            (12_000_000, 18_000_000, 36_000_000)
        );
        assert_eq!(s.option("maxrate"), Some("18000000"));
        assert_eq!(s.option("bufsize"), Some("36000000"));
        assert_eq!(s.option("x264-params"), Some("open-gop=0"));
    }

    #[test]
    fn a_keyframe_comes_at_least_once_a_second() {
        let keyint = |rate| {
            EncoderSetup::software(Codec::Hevc, Quality::High, WIDE_1080, rate, false).keyint
        };
        assert_eq!(keyint(FrameRate::FPS_30), 30);
        assert_eq!(keyint(FrameRate::FPS_29_97), 29);
        assert_eq!(keyint(FrameRate::FPS_23_976), 23);
        assert_eq!(keyint(FrameRate::FPS_60), 60);
    }

    #[test]
    fn ten_bit_is_for_hevc_and_av1_only() {
        let rate = FrameRate::FPS_30;
        let s = |codec, ten| EncoderSetup::software(codec, Quality::High, WIDE_1080, rate, ten);
        assert_eq!(s(Codec::H264, true).yuv, YuvLayout::I420);
        assert_eq!(s(Codec::Hevc, true).yuv, YuvLayout::I010);
        assert_eq!(s(Codec::Hevc, true).layout, PixelLayout::Yuv420p10);
        assert_eq!(s(Codec::Av1, true).yuv, YuvLayout::I010);
        assert_eq!(s(Codec::Hevc, false).yuv, YuvLayout::I420);
        assert_eq!(s(Codec::Hevc, false).color.chroma_location, CHROMA_LEFT);
    }
}
