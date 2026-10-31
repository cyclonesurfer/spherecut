//! The encoders and how they are set (M2 plan section 8.3, amendment 2
//! items 24 and 49): VA-API's in VBR at the preset's bitrate, the
//! software ones in CRF with the preset's bitrate as a VBV cap; a
//! keyframe every second in closed GOPs, BT.709 limited range with left
//! chroma siting (what the YUV pass writes).

use spherecut_core::project::{Codec, Quality};
use spherecut_core::time::{FrameRate, Rounding, Time};
use spherecut_media::encode::{ColorTags, PixelLayout};
use spherecut_render::yuv::YuvLayout;

use crate::preset::Preset;

/// FFmpeg's `AVCHROMA_LOC_LEFT`: the YUV pass sites chroma left.
pub const CHROMA_LEFT: i32 = 1;

/// Mesa's reading of VA-API's quality level, which FFmpeg's
/// `compression_level` sets (research note encode.md, "Quality level"):
/// bit 0 says the rest is set, bits 1 and 2 are the preset (0 speed, 1
/// balanced, 2 quality, 3 high quality), bit 3 is pre-encode and bit 4
/// VBAQ, the adaptive quantisation.
pub mod quality_level {
    pub const VALID: u32 = 1;
    pub const BALANCED: u32 = 1 << 1;
    pub const QUALITY: u32 = 2 << 1;
    pub const HIGH_QUALITY: u32 = 3 << 1;
    pub const PRE_ENCODE: u32 = 1 << 3;
    pub const VBAQ: u32 = 1 << 4;
}

/// Where an encoder runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    /// On the GPU's VA-API device, from NV12 or P010 surfaces.
    Vaapi,
    /// On the CPU, from I420 or I010.
    Software,
}
/// How the export encodes: which encoder, its options, and the planes
/// the YUV pass writes for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderSetup {
    /// FFmpeg's name for it, such as `libx265`.
    pub name: &'static str,
    pub kind: EncoderKind,
    /// What the YUV pass writes and the encoder takes: NV12 or P010 for
    /// VA-API, I420 or I010 for software.
    pub yuv: YuvLayout,
    /// The same as the writer's layout.
    pub layout: PixelLayout,
    /// The preset's bitrate, in bits a second.
    pub bitrate: u64,
    /// The cap: 1.5 × the bitrate.
    pub maxrate: u64,
    /// The rate control's buffer: two seconds of `maxrate`, without which
    /// x264 and x265 ignore the cap (amendment 2 item 24).
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
        let Rates {
            bitrate,
            maxrate,
            bufsize,
            keyint,
        } = Rates::of(codec, quality, preset, rate);
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
            kind: EncoderKind::Software,
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

    /// VA-API's encoder for `codec` (h264_vaapi, hevc_vaapi or
    /// av1_vaapi) at `quality`, for `preset` at `rate`: VBR at the
    /// preset's bitrate with `maxrate` 1.5 times it, and Mesa's quality
    /// level by quality (quality preset with pre-encode and VBAQ for High,
    /// balanced for Medium and Small). With a bitrate set, av1_vaapi's QP
    /// (on a 0 to 255 scale) is never used (amendment 2 item 49). 10-bit
    /// is P010, which picks HEVC's Main10 and AV1's 10-bit profile; H.264
    /// stays 8-bit, as VA-API's drivers encode it.
    pub fn vaapi(
        codec: Codec,
        quality: Quality,
        preset: Preset,
        rate: FrameRate,
        ten_bit: bool,
    ) -> EncoderSetup {
        use quality_level::*;
        let ten_bit = ten_bit && codec != Codec::H264;
        let Rates {
            bitrate,
            maxrate,
            bufsize,
            keyint,
        } = Rates::of(codec, quality, preset, rate);
        let name = match codec {
            Codec::H264 => "h264_vaapi",
            Codec::Hevc => "hevc_vaapi",
            Codec::Av1 => "av1_vaapi",
        };
        let level = match quality {
            Quality::High => VALID | QUALITY | PRE_ENCODE | VBAQ,
            Quality::Medium | Quality::Small => VALID | BALANCED,
        };
        let options: Vec<(String, String)> = vec![
            ("rc_mode".into(), "VBR".into()),
            ("b".into(), bitrate.to_string()),
            ("maxrate".into(), maxrate.to_string()),
            ("bufsize".into(), bufsize.to_string()),
            ("g".into(), keyint.to_string()),
            // Every GOP starts at an IDR frame (idr_interval 0, FFmpeg's
            // default), and none refers across it.
            ("flags".into(), "+cgop".into()),
            ("compression_level".into(), level.to_string()),
        ];
        let (yuv, layout) = if ten_bit {
            (YuvLayout::P010, PixelLayout::P010)
        } else {
            (YuvLayout::Nv12, PixelLayout::Nv12)
        };
        EncoderSetup {
            name,
            kind: EncoderKind::Vaapi,
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

    /// The encoder of `kind` for `codec`, as [`vaapi`](Self::vaapi) or
    /// [`software`](Self::software) sets it.
    pub fn of(
        kind: EncoderKind,
        codec: Codec,
        quality: Quality,
        preset: Preset,
        rate: FrameRate,
        ten_bit: bool,
    ) -> EncoderSetup {
        match kind {
            EncoderKind::Vaapi => Self::vaapi(codec, quality, preset, rate, ten_bit),
            EncoderKind::Software => Self::software(codec, quality, preset, rate, ten_bit),
        }
    }

    /// The same encoder for `--draft` (M2 plan section 9): its fastest
    /// settings (x264's and x265's `ultrafast`, SVT-AV1's preset 12,
    /// Mesa's speed preset), and the bitrates scaled from the preset's
    /// `full` size to the draft's `size` by their pixels.
    pub fn draft(mut self, full: (u32, u32), size: (u32, u32)) -> EncoderSetup {
        let pixels = |(w, h): (u32, u32)| u128::from(w) * u128::from(h);
        let scaled = u128::from(self.bitrate) * pixels(size) / pixels(full).max(1);
        self.bitrate = u64::try_from(scaled).unwrap_or(u64::MAX).max(1);
        self.maxrate = self.bitrate * 3 / 2;
        self.bufsize = self.maxrate * 2;
        let speed = match self.name {
            "libx264" | "libx265" => Some("ultrafast"),
            "libsvtav1" => Some("12"),
            _ => None,
        };
        let (bitrate, maxrate, bufsize) = (self.bitrate, self.maxrate, self.bufsize);
        for (k, v) in &mut self.options {
            match k.as_str() {
                "b" => *v = bitrate.to_string(),
                "maxrate" => *v = maxrate.to_string(),
                "bufsize" => *v = bufsize.to_string(),
                "preset" => {
                    if let Some(speed) = speed {
                        *v = speed.into();
                    }
                }
                "compression_level" => *v = quality_level::VALID.to_string(),
                _ => {}
            }
        }
        self
    }

    /// The same encoder at Mesa's quality level `level`
    /// ([`quality_level`]); a software encoder is left as it is.
    pub fn with_vaapi_level(mut self, level: u32) -> EncoderSetup {
        for (k, v) in &mut self.options {
            if k == "compression_level" {
                *v = level.to_string();
            }
        }
        self
    }

    /// Whether the export's video is 10-bit with this encoder.
    pub fn ten_bit(&self) -> bool {
        self.layout.ten_bit()
    }

    /// The most bytes a file of `duration` should take, with
    /// `audio_bits` a second of audio (section 8.6, amendment 2 item
    /// 24): the video at `maxrate` and one `bufsize` beyond it, since the
    /// encoder may start with its buffer full and spend it, a tenth more
    /// for the container, and the audio. The free-space check asks for
    /// two of these.
    pub fn estimate(&self, audio_bits: u64, duration: Time) -> u64 {
        let secs = duration.as_rational();
        let (num, den) = (
            u128::try_from(secs.num()).unwrap_or(0),
            u128::try_from(secs.den()).unwrap_or(1),
        );
        // In bits times `den`, so that nothing rounds until the end.
        let video = (u128::from(self.maxrate) * num + u128::from(self.bufsize) * den) * 11 / 10;
        let bits = video + u128::from(audio_bits) * num;
        u64::try_from(bits.div_ceil(den * 8)).unwrap_or(u64::MAX)
    }

    /// The option `key`'s value, if it is set.
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// What every encoder is given: the preset's bitrate, the cap and its
/// buffer, and the keyframe interval.
struct Rates {
    bitrate: u64,
    maxrate: u64,
    bufsize: u64,
    keyint: u32,
}

impl Rates {
    fn of(codec: Codec, quality: Quality, preset: Preset, rate: FrameRate) -> Rates {
        let bitrate = preset.bitrate(quality, codec, rate);
        let maxrate = bitrate * 3 / 2;
        Rates {
            bitrate,
            maxrate,
            bufsize: maxrate * 2,
            // At least one frame, at most a second's worth.
            keyint: rate
                .as_rational()
                .round(Rounding::Floor)
                .clamp(1, i64::from(u32::MAX)) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::Aspect;
    use spherecut_core::time::Rational;

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
    fn a_draft_is_fastest_at_a_quarter_of_the_bits() {
        let full = WIDE_1080.size();
        let draft = WIDE_1080.draft_size();
        let rate = FrameRate::FPS_30;
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            let s = EncoderSetup::software(codec, Quality::High, WIDE_1080, rate, false);
            let d = s.clone().draft(full, draft);
            // 960 × 544 of 1920 × 1080.
            assert_eq!(
                d.bitrate,
                s.bitrate * 960 * 544 / (1920 * 1080),
                "{}",
                s.name
            );
            assert_eq!(
                (d.maxrate, d.bufsize),
                (d.bitrate * 3 / 2, d.bitrate * 3 / 2 * 2)
            );
            assert_eq!(d.option("maxrate"), Some(d.maxrate.to_string().as_str()));
            assert_eq!(d.option("bufsize"), Some(d.bufsize.to_string().as_str()));
            let fastest = if codec == Codec::Av1 {
                "12"
            } else {
                "ultrafast"
            };
            assert_eq!(d.option("preset"), Some(fastest), "{}", s.name);
            assert_eq!((d.option("crf"), d.keyint), (s.option("crf"), s.keyint));
            let v = EncoderSetup::vaapi(codec, Quality::High, WIDE_1080, rate, false);
            let dv = v.draft(full, draft);
            assert_eq!(dv.option("compression_level"), Some("1"));
            assert_eq!(dv.option("b"), Some(dv.bitrate.to_string().as_str()));
        }
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
    fn the_estimate_is_the_cap_and_a_buffer_with_a_tenth_more_and_the_audio() {
        // 360-3840 in H.264 at High and 30 fps is 45 Mb/s: a cap of
        // 67.5 Mb/s and a buffer of 135 Mb. For 10 s, (675 + 135) Mb with
        // a tenth more is 891 Mb, and 256 kb/s of audio 2.56 Mb.
        let sphere = Preset::Sphere { width: 3840 };
        let s =
            EncoderSetup::software(Codec::H264, Quality::High, sphere, FrameRate::FPS_30, false);
        assert_eq!((s.maxrate, s.bufsize), (67_500_000, 135_000_000));
        assert_eq!(s.estimate(256_000, Time::from_secs(10)), 111_695_000);
        // A third of a second: (22.5 + 135) Mb with a tenth more, exactly.
        let third = Time::seconds(Rational::new(1, 3).unwrap());
        assert_eq!(s.estimate(0, third), 21_656_250);
        // However short, a buffer's worth.
        assert_eq!(s.estimate(0, Time::ZERO), 18_562_500);
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

    #[test]
    fn vaapi_runs_vbr_at_the_bitrate_with_the_same_cap_in_nv12_or_p010() {
        let rate = FrameRate::FPS_30;
        let sphere = Preset::Sphere { width: 3840 };
        let s = EncoderSetup::vaapi(Codec::Hevc, Quality::High, sphere, rate, true);
        assert_eq!((s.name, s.kind), ("hevc_vaapi", EncoderKind::Vaapi));
        // 45 Mb/s by 0.7 for HEVC.
        assert_eq!(
            (s.bitrate, s.maxrate, s.bufsize),
            (31_500_000, 47_250_000, 94_500_000)
        );
        assert_eq!(s.option("rc_mode"), Some("VBR"));
        assert_eq!(s.option("b"), Some("31500000"));
        assert_eq!(s.option("maxrate"), Some("47250000"));
        assert_eq!(s.option("g"), Some("30"));
        // Quality preset (2 << 1), pre-encode (8) and VBAQ (16), valid.
        assert_eq!(s.option("compression_level"), Some("29"));
        assert_eq!((s.yuv, s.layout), (YuvLayout::P010, PixelLayout::P010));
        assert!(s.ten_bit());
        // Balanced for the others; no QP, which VBR doesn't read.
        for q in [Quality::Medium, Quality::Small] {
            let s = EncoderSetup::vaapi(Codec::Av1, q, sphere, rate, false);
            assert_eq!(s.name, "av1_vaapi");
            assert_eq!(s.option("compression_level"), Some("3"), "{q:?}");
            assert_eq!(s.option("qp"), None);
            assert_eq!(s.yuv, YuvLayout::Nv12);
        }
        // H.264 is 8-bit, from a 10-bit source too.
        let s = EncoderSetup::vaapi(Codec::H264, Quality::High, WIDE_1080, rate, true);
        assert_eq!(
            (s.name, s.yuv, s.ten_bit()),
            ("h264_vaapi", YuvLayout::Nv12, false)
        );
        // The same rates as the software encoder's.
        let soft = EncoderSetup::software(Codec::H264, Quality::High, WIDE_1080, rate, true);
        assert_eq!(
            (s.bitrate, s.maxrate, s.bufsize, s.keyint),
            (soft.bitrate, soft.maxrate, soft.bufsize, soft.keyint)
        );
        assert_eq!(
            EncoderSetup::of(
                EncoderKind::Software,
                Codec::H264,
                Quality::High,
                WIDE_1080,
                rate,
                true
            ),
            soft
        );
        // A level given is the one used; software has none to set.
        let s = s.with_vaapi_level(quality_level::VALID | quality_level::QUALITY);
        assert_eq!(s.option("compression_level"), Some("5"));
        assert_eq!(soft.clone().with_vaapi_level(5), soft);
    }
}
