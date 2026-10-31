//! The export presets and their bitrates (M2 plan section 8.1).
//!
//! A preset is an output's shape: a flat aspect at one of three short
//! sides, or a 2:1 sphere at one of three widths. Every size is a
//! multiple of 4 wide and even tall, as the YUV pass needs. The bitrates
//! start from YouTube's upload guidance for H.264 at 30 fps (4K30 SDR 35
//! to 45 Mb/s, 8K30 80 to 160) and scale by rate / 30 up to 30 fps and
//! half as steeply above it, so that 60 fps gets 1.5 times 30's as
//! YouTube's own figures do (amendment 2 item 47), by 0.7 for HEVC and by
//! 0.6 for AV1. They are the hardware encoders' target, the VBV cap's base
//! for the software ones (section 8.3) and the free-space estimate's
//! (section 8.6).

use std::fmt;

use spherecut_core::project::{Aspect, Codec, Output, Quality};
use spherecut_core::time::FrameRate;

/// A row of section 8.1's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// `flat-16x9-1080` and the rest: `short_side` is 1080, 1440 or 2160.
    Flat { aspect: Aspect, short_side: u32 },
    /// `360-3840`, `360-5760` or `360-7680`: `width` × `width / 2`.
    Sphere { width: u32 },
}

/// The flat presets' short sides.
pub const SHORT_SIDES: [u32; 3] = [1080, 1440, 2160];

/// The sphere presets' widths.
pub const SPHERE_WIDTHS: [u32; 3] = [3840, 5760, 7680];

impl Preset {
    /// Every preset: the flat ones by aspect, then by size, then the
    /// spheres.
    pub fn all() -> Vec<Preset> {
        let flat = Aspect::ALL.into_iter().flat_map(|aspect| {
            SHORT_SIDES
                .into_iter()
                .map(move |short_side| Preset::Flat { aspect, short_side })
        });
        let sphere = SPHERE_WIDTHS
            .into_iter()
            .map(|width| Preset::Sphere { width });
        flat.chain(sphere).collect()
    }

    /// The preset an output is, `None` for a size that isn't one.
    pub fn of(output: &Output) -> Option<Preset> {
        let p = match *output {
            Output::Flat { aspect, short_side } => Preset::Flat { aspect, short_side },
            Output::Equirect { width, .. } => Preset::Sphere { width },
        };
        Preset::all().contains(&p).then_some(p)
    }

    /// The preset `name` names, as [`Preset`]'s `Display` writes it.
    pub fn parse(name: &str) -> Option<Preset> {
        Preset::all().into_iter().find(|p| p.to_string() == name)
    }

    /// Width and height in pixels.
    pub fn size(self) -> (u32, u32) {
        match self {
            Preset::Flat { aspect, short_side } => Output::Flat { aspect, short_side }.size(),
            Preset::Sphere { width } => (width, width / 2),
        }
    }

    /// The size `--draft` renders at (M2 plan section 9, amendment 2
    /// item 23): half of each side, rounded up to a multiple of 8, so
    /// that 4:5 at 1080 drafts at 544 × 680 rather than 540 × 675. The
    /// aspect moves by under 1 %.
    pub fn draft_size(self) -> (u32, u32) {
        let (w, h) = self.size();
        let half = |side: u32| side.div_ceil(16) * 8;
        (half(w), half(h))
    }

    /// The bitrate in bits a second for `quality` in `codec` at `rate`:
    /// section 8.1's table, scaled. Above 30 fps the rate's factor is
    /// (rate + 30) / 60: 1.5 at 60, 1.33 at 50.
    pub fn bitrate(self, quality: Quality, codec: Codec, rate: FrameRate) -> u64 {
        let base_kbps = self.base_kbps(quality);
        // Each factor as a fraction: the aspect's, the codec's, and the
        // rate's over 30 fps.
        let aspect: (u128, u128) = match self {
            Preset::Flat {
                aspect: Aspect::Square,
                ..
            } => (6, 10),
            Preset::Flat {
                aspect: Aspect::Tall,
                ..
            } => (7, 10),
            _ => (1, 1),
        };
        let codec: (u128, u128) = match codec {
            Codec::H264 => (1, 1),
            Codec::Hevc => (7, 10),
            Codec::Av1 => (6, 10),
        };
        let r = rate.as_rational();
        // A frame rate is positive, so both are.
        let (rn, rd) = (
            r.num().unsigned_abs() as u128,
            r.den().unsigned_abs() as u128,
        );
        let rate: (u128, u128) = if rn <= 30 * rd {
            (rn, 30 * rd)
        } else {
            (rn + 30 * rd, 60 * rd)
        };
        let num = u128::from(base_kbps) * 1000 * aspect.0 * codec.0 * rate.0;
        let den = aspect.1 * codec.1 * rate.1;
        // To the nearest bit a second.
        ((num + den / 2) / den) as u64
    }

    /// H.264 at 30 fps, in kb/s: section 8.1's table.
    fn base_kbps(self, quality: Quality) -> u64 {
        let tier = |high, medium, small| match quality {
            Quality::High => high,
            Quality::Medium => medium,
            Quality::Small => small,
        };
        match self {
            Preset::Flat { short_side, .. } => match short_side {
                1080 => tier(12_000, 8_000, 5_000),
                1440 => tier(24_000, 16_000, 10_000),
                _ => tier(45_000, 35_000, 20_000),
            },
            Preset::Sphere { width } => match width {
                3840 => tier(45_000, 35_000, 20_000),
                5760 => tier(80_000, 60_000, 40_000),
                _ => tier(160_000, 100_000, 60_000),
            },
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Preset::Flat { aspect, short_side } => {
                let (w, h) = aspect.ratio();
                write!(f, "flat-{w}x{h}-{short_side}")
            }
            Preset::Sphere { width } => write!(f, "360-{width}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::Orientation;

    use super::*;

    const MB: u64 = 1_000_000;

    #[test]
    fn every_preset_has_a_name_that_parses_back_and_a_size_the_yuv_pass_takes() {
        let all = Preset::all();
        assert_eq!(all.len(), 15);
        for p in &all {
            let name = p.to_string();
            assert_eq!(Preset::parse(&name), Some(*p), "{name}");
            let (w, h) = p.size();
            assert!(
                w.is_multiple_of(4) && h.is_multiple_of(2),
                "{name}: {w}x{h}"
            );
        }
        let names: Vec<String> = all.iter().map(Preset::to_string).collect();
        for want in [
            "flat-16x9-1080",
            "flat-9x16-2160",
            "flat-1x1-1440",
            "flat-4x5-1080",
            "360-3840",
            "360-7680",
        ] {
            assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
        }
        assert_eq!(Preset::parse("flat-16x9-720"), None);
        assert_eq!(Preset::parse("360-4096"), None);
        let sizes = [
            ("flat-16x9-2160", (3840, 2160)),
            ("flat-9x16-1080", (1080, 1920)),
            ("flat-1x1-1440", (1440, 1440)),
            ("flat-4x5-2160", (2160, 2700)),
            ("360-5760", (5760, 2880)),
        ];
        for (name, size) in sizes {
            assert_eq!(Preset::parse(name).unwrap().size(), size, "{name}");
        }
    }

    #[test]
    fn drafts_are_half_each_side_rounded_up_to_a_multiple_of_8() {
        let draft = |name| Preset::parse(name).unwrap().draft_size();
        assert_eq!(draft("flat-4x5-1080"), (544, 680));
        assert_eq!(draft("flat-16x9-1080"), (960, 544));
        assert_eq!(draft("flat-9x16-2160"), (1080, 1920));
        assert_eq!(draft("360-3840"), (1920, 960));
        assert_eq!(draft("360-5760"), (2880, 1440));
        for p in Preset::all() {
            let ((w, h), (dw, dh)) = (p.size(), p.draft_size());
            assert!(dw.is_multiple_of(8) && dh.is_multiple_of(8), "{p}");
            assert!(2 * dw >= w && 2 * dw < w + 16, "{p}");
            assert!(2 * dh >= h && 2 * dh < h + 16, "{p}");
            let aspect = |w: u32, h: u32| f64::from(w) / f64::from(h);
            assert!((aspect(dw, dh) / aspect(w, h) - 1.0).abs() < 0.01, "{p}");
        }
    }

    #[test]
    fn an_output_is_a_preset_only_at_a_listed_size() {
        let flat = Output::Flat {
            aspect: Aspect::Portrait,
            short_side: 1440,
        };
        assert_eq!(
            Preset::of(&flat),
            Some(Preset::Flat {
                aspect: Aspect::Portrait,
                short_side: 1440
            })
        );
        let sphere = Output::Equirect {
            width: 7680,
            orientation: Orientation::IDENTITY,
        };
        assert_eq!(Preset::of(&sphere), Some(Preset::Sphere { width: 7680 }));
        let odd = Output::Flat {
            aspect: Aspect::Wide,
            short_side: 720,
        };
        assert_eq!(Preset::of(&odd), None);
    }

    #[test]
    fn bitrates_follow_the_table_and_scale_by_aspect_codec_and_rate() {
        let p = |name| Preset::parse(name).unwrap();
        let fps30 = FrameRate::FPS_30;
        let h264 = |name, q| p(name).bitrate(q, Codec::H264, fps30);
        // The table, H.264 at 30 fps.
        assert_eq!(h264("flat-16x9-1080", Quality::High), 12 * MB);
        assert_eq!(h264("flat-16x9-1440", Quality::Medium), 16 * MB);
        assert_eq!(h264("flat-16x9-2160", Quality::Small), 20 * MB);
        assert_eq!(h264("360-3840", Quality::High), 45 * MB);
        assert_eq!(h264("360-5760", Quality::Medium), 60 * MB);
        assert_eq!(h264("360-7680", Quality::High), 160 * MB);
        assert_eq!(h264("360-7680", Quality::Small), 60 * MB);
        // 9:16 as 16:9 of the same pixels; 1:1 and 4:5 a share of it.
        assert_eq!(h264("flat-9x16-1080", Quality::High), 12 * MB);
        assert_eq!(h264("flat-1x1-1080", Quality::High), 7_200_000);
        assert_eq!(h264("flat-4x5-2160", Quality::High), 31_500_000);
        // HEVC 0.7 and AV1 0.6 of H.264.
        let q = Quality::High;
        assert_eq!(p("360-7680").bitrate(q, Codec::Hevc, fps30), 112 * MB);
        assert_eq!(p("360-7680").bitrate(q, Codec::Av1, fps30), 96 * MB);
        // The rate over 30 up to 30 fps: 1000/1001 of it at 29.97 (to the
        // nearest bit a second). Above, (rate + 30) / 60: 1.5 times 30's
        // at 60, as YouTube has it, not twice.
        let at = |rate| p("flat-16x9-1080").bitrate(q, Codec::H264, rate);
        assert_eq!(at(FrameRate::FPS_60), 18 * MB);
        assert_eq!(at(FrameRate::FPS_50), 16 * MB);
        assert_eq!(at(FrameRate::FPS_30), 12 * MB);
        assert_eq!(at(FrameRate::FPS_29_97), 11_988_012);
        assert_eq!(at(FrameRate::FPS_24), 9_600_000);
        // 59.94 is (60000/1001 + 30) / 60 of 12 Mb/s.
        assert_eq!(at(FrameRate::FPS_59_94), 17_988_012);
        assert_eq!(
            p("360-3840").bitrate(q, Codec::Hevc, FrameRate::FPS_60),
            47_250_000
        );
    }
}
