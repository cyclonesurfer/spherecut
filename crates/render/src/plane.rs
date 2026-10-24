//! The canonical plane textures and their mip chains (M1 plan section
//! 4.2, DECISIONS D31).
//!
//! | Source plane | Uploaded as | Canonical (sampled, mipped) |
//! |---|---|---|
//! | 8-bit, one component | `R8Unorm`, directly | the same |
//! | 8-bit, interleaved UV | `Rg8Unorm`, directly | the same |
//! | 9 to 16 bits, one component | `R16Uint` scratch | `R16Float`, code / (2^b − 1) |
//! | 9 to 16 bits, interleaved UV | `Rg16Uint` scratch | `Rg16Float` |
//!
//! Every one of these formats is guaranteed by wgpu without optional
//! features: the canonical ones renderable and filterable, the scratch
//! ones as copy destinations read with `textureLoad`.

use spherecut_core::video::{ColorFamily, PixelLayout, SampleBytes};

use crate::error::RenderError;

/// A canonical plane texture's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneFormat {
    R8,
    Rg8,
    R16F,
    Rg16F,
}

impl PlaneFormat {
    pub const ALL: [PlaneFormat; 4] = [
        PlaneFormat::R8,
        PlaneFormat::Rg8,
        PlaneFormat::R16F,
        PlaneFormat::Rg16F,
    ];

    pub fn texture(self) -> wgpu::TextureFormat {
        match self {
            PlaneFormat::R8 => wgpu::TextureFormat::R8Unorm,
            PlaneFormat::Rg8 => wgpu::TextureFormat::Rg8Unorm,
            PlaneFormat::R16F => wgpu::TextureFormat::R16Float,
            PlaneFormat::Rg16F => wgpu::TextureFormat::Rg16Float,
        }
    }

    /// The integer format a deep plane is uploaded in, before the ingest
    /// pass; `None` for 8-bit planes, which are uploaded as they are.
    pub fn scratch(self) -> Option<wgpu::TextureFormat> {
        match self {
            PlaneFormat::R8 | PlaneFormat::Rg8 => None,
            PlaneFormat::R16F => Some(wgpu::TextureFormat::R16Uint),
            PlaneFormat::Rg16F => Some(wgpu::TextureFormat::Rg16Uint),
        }
    }

    pub fn components(self) -> u32 {
        match self {
            PlaneFormat::R8 | PlaneFormat::R16F => 1,
            PlaneFormat::Rg8 | PlaneFormat::Rg16F => 2,
        }
    }

    /// Bytes per texel of the canonical texture, and of the source
    /// samples it is uploaded from.
    pub fn bytes_per_texel(self) -> u32 {
        match self {
            PlaneFormat::R8 => 1,
            PlaneFormat::Rg8 | PlaneFormat::R16F => 2,
            PlaneFormat::Rg16F => 4,
        }
    }

    pub fn is_deep(self) -> bool {
        self.scratch().is_some()
    }
}

/// The mip levels a plane of `w` × `h` gets: the chain stops near 64
/// pixels, `clamp(1 + floor(log2(max(w, h) / 64)), 1, 8)`. A 5760×2880
/// luma plane gets 7 and its 2880×1440 chroma 6, both ending at 90×45.
pub fn mip_levels(w: u32, h: u32) -> u32 {
    let m = w.max(h);
    let mut n = 1;
    while n < 8 && (m >> n) >= 64 {
        n += 1;
    }
    n
}

/// The most levels a `w` × `h` texture can have.
pub fn full_chain(w: u32, h: u32) -> u32 {
    32 - w.max(h).max(1).leading_zeros()
}

/// One plane of a [`SlotShape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneShape {
    pub format: PlaneFormat,
    pub size: (u32, u32),
    pub levels: u32,
}

/// How the shader picks a frame's samples ([`SlotShape::layout_code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutKind {
    /// Y, then U and V interleaved (NV12, P010).
    SemiPlanar,
    /// Y, U, V.
    Planar,
    Gray,
    /// G, B, R.
    Gbr,
}

/// What a GPU slot holds: the canonical texture of each plane, with its
/// size and mip count. Two frames with the same shape share slots,
/// whatever FFmpeg calls their formats.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotShape {
    /// The picture's size, which is the first plane's.
    pub size: (u32, u32),
    pub kind: LayoutKind,
    pub planes: Vec<PlaneShape>,
    /// Significant bits per sample, and whether they are the high ones
    /// of each 16-bit sample (P010).
    pub bit_depth: u8,
    pub msb_aligned: bool,
}

impl SlotShape {
    /// The shape of a `size` picture in `layout`, with [`mip_levels`] per
    /// plane. `Invalid` for a layout the renderer can't sample: other
    /// plane counts, chroma that isn't U then V, depths outside 8 to 16.
    pub fn of(layout: &PixelLayout, size: (u32, u32)) -> Result<SlotShape, RenderError> {
        let invalid = |why: &str| {
            RenderError::Invalid(format!("{} {}x{}: {why}", layout.name, size.0, size.1))
        };
        if size.0 == 0 || size.1 == 0 {
            return Err(invalid("a picture with no pixels"));
        }
        let comps: Vec<u8> = layout.planes.iter().map(|p| p.components).collect();
        let kind = match (layout.family, comps.as_slice()) {
            (ColorFamily::Yuv, [1, 2]) => LayoutKind::SemiPlanar,
            (ColorFamily::Yuv, [1, 1, 1]) => LayoutKind::Planar,
            (ColorFamily::Gray, [1]) => LayoutKind::Gray,
            (ColorFamily::Rgb, [1, 1, 1]) => LayoutKind::Gbr,
            _ => return Err(invalid("not a plane layout the renderer samples")),
        };
        if layout.planes[0].log2_sub != (0, 0)
            || (kind == LayoutKind::Gbr && layout.planes.iter().any(|p| p.log2_sub != (0, 0)))
            || (kind == LayoutKind::Planar
                && layout.planes[1].log2_sub != layout.planes[2].log2_sub)
        {
            return Err(invalid(
                "planes subsampled in a way the renderer doesn't sample",
            ));
        }
        let deep = match (layout.bit_depth, layout.planes[0].bytes) {
            (8, SampleBytes::One) => false,
            (9..=16, SampleBytes::Two) => true,
            _ => {
                return Err(invalid(
                    "a sample depth other than 8 bits in bytes or 9 to 16 in words",
                ));
            }
        };
        if layout
            .planes
            .iter()
            .any(|p| p.bytes != layout.planes[0].bytes)
        {
            return Err(invalid("planes of different sample sizes"));
        }
        let planes = layout
            .planes
            .iter()
            .map(|p| {
                let format = match (p.components, deep) {
                    (1, false) => PlaneFormat::R8,
                    (2, false) => PlaneFormat::Rg8,
                    (1, true) => PlaneFormat::R16F,
                    _ => PlaneFormat::Rg16F,
                };
                let (w, h) = p.size(size.0, size.1);
                PlaneShape {
                    format,
                    size: (w, h),
                    levels: mip_levels(w, h),
                }
            })
            .collect();
        Ok(SlotShape {
            size,
            kind,
            planes,
            bit_depth: layout.bit_depth,
            msb_aligned: layout.msb_aligned && layout.bit_depth < 16,
        })
    }

    /// The same shape with `levels` mip levels per plane, or as many as a
    /// plane can have: for tests of the mip pass at sizes the 64-pixel cap
    /// would leave with one level.
    pub fn with_levels(mut self, levels: u32) -> SlotShape {
        for p in &mut self.planes {
            p.levels = levels.clamp(1, full_chain(p.size.0, p.size.1));
        }
        self
    }

    /// Whether a frame of shape `other` can be uploaded into a slot of
    /// this one: the same planes at the same sizes. Mip counts and the
    /// depth don't matter, since the ingest pass normalises any depth.
    pub fn fits(&self, other: &SlotShape) -> bool {
        self.size == other.size
            && self.kind == other.kind
            && self.planes.len() == other.planes.len()
            && self
                .planes
                .iter()
                .zip(&other.planes)
                .all(|(a, b)| a.format == b.format && a.size == b.size)
    }

    /// The shader's `layout` code.
    pub fn layout_code(&self) -> u32 {
        match self.kind {
            LayoutKind::SemiPlanar => 0,
            LayoutKind::Planar => 1,
            LayoutKind::Gray => 2,
            LayoutKind::Gbr => 3,
        }
    }

    /// Bytes of GPU memory the canonical textures take, mips included.
    pub fn texture_bytes(&self) -> u64 {
        self.planes
            .iter()
            .map(|p| {
                (0..p.levels)
                    .map(|l| {
                        let (w, h) = ((p.size.0 >> l).max(1), (p.size.1 >> l).max(1));
                        u64::from(w) * u64::from(h) * u64::from(p.format.bytes_per_texel())
                    })
                    .sum::<u64>()
            })
            .sum()
    }

    /// For messages: "3 planes, 5760x2880, 10-bit".
    pub fn describe(&self) -> String {
        let planes: Vec<String> = self
            .planes
            .iter()
            .map(|p| format!("{:?} {}x{}", p.format, p.size.0, p.size.1))
            .collect();
        format!(
            "{}-bit {:?} [{}]",
            self.bit_depth,
            self.kind,
            planes.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spherecut_core::video::PlaneDesc;

    fn layout(name: &str, comps: &[(u8, (u8, u8))], bytes: SampleBytes, depth: u8) -> PixelLayout {
        PixelLayout {
            name: name.into(),
            planes: comps
                .iter()
                .map(|&(components, log2_sub)| PlaneDesc {
                    components,
                    bytes,
                    log2_sub,
                })
                .collect(),
            family: ColorFamily::Yuv,
            bit_depth: depth,
            msb_aligned: name.starts_with('p'),
        }
    }

    #[test]
    fn mip_chains_stop_near_64_pixels() {
        assert_eq!(mip_levels(5760, 2880), 7);
        assert_eq!(mip_levels(2880, 1440), 6);
        assert_eq!((5760 >> 6, 2880 >> 5), (90, 90));
        assert_eq!(mip_levels(7680, 3840), 7);
        assert_eq!(mip_levels(16384, 8192), 8);
        assert_eq!(mip_levels(1024, 512), 5);
        assert_eq!(mip_levels(127, 64), 1);
        assert_eq!(mip_levels(128, 64), 2);
        assert_eq!(mip_levels(1, 1), 1);
        assert_eq!(full_chain(45, 23), 6);
        assert_eq!(full_chain(1, 1), 1);
    }

    #[test]
    fn layouts_map_to_canonical_planes() {
        let nv12 = layout("nv12", &[(1, (0, 0)), (2, (1, 1))], SampleBytes::One, 8);
        let s = SlotShape::of(&nv12, (5760, 2880)).unwrap();
        assert_eq!(s.kind, LayoutKind::SemiPlanar);
        assert_eq!(s.planes[0].format, PlaneFormat::R8);
        assert_eq!(s.planes[1].format, PlaneFormat::Rg8);
        assert_eq!(s.planes[1].size, (2880, 1440));
        assert_eq!((s.planes[0].levels, s.planes[1].levels), (7, 6));
        // About 33 MB with mips, as the plan says.
        assert!(
            (32_000_000..34_000_000).contains(&s.texture_bytes()),
            "{}",
            s.texture_bytes()
        );

        let p010 = layout("p010le", &[(1, (0, 0)), (2, (1, 1))], SampleBytes::Two, 10);
        let s = SlotShape::of(&p010, (5, 3)).unwrap();
        assert_eq!(s.planes[1].format, PlaneFormat::Rg16F);
        assert_eq!(s.planes[1].size, (3, 2));
        assert!(s.msb_aligned);

        let i420p10 = layout(
            "yuv420p10le",
            &[(1, (0, 0)), (1, (1, 1)), (1, (1, 1))],
            SampleBytes::Two,
            10,
        );
        let a = SlotShape::of(&i420p10, (64, 32)).unwrap();
        assert_eq!(a.kind, LayoutKind::Planar);
        assert!(!a.msb_aligned);
        // Another depth of the same planes fits the same slot.
        let i420p12 = PixelLayout {
            bit_depth: 12,
            ..i420p10.clone()
        };
        assert!(a.fits(&SlotShape::of(&i420p12, (64, 32)).unwrap()));
        assert!(!a.fits(&SlotShape::of(&i420p10, (64, 34)).unwrap()));
        assert!(!a.fits(&SlotShape::of(&p010, (64, 32)).unwrap()));

        // Forced levels stop at what a plane can have.
        let s = SlotShape::of(&i420p10, (45, 23)).unwrap().with_levels(9);
        assert_eq!((s.planes[0].levels, s.planes[1].levels), (6, 5));
    }

    #[test]
    fn layouts_it_cant_sample_are_refused() {
        let vu = layout(
            "odd",
            &[(1, (0, 0)), (1, (1, 1)), (1, (1, 0))],
            SampleBytes::One,
            8,
        );
        assert!(SlotShape::of(&vu, (64, 32)).is_err());
        let deep_bytes = layout("x", &[(1, (0, 0))], SampleBytes::One, 10);
        assert!(SlotShape::of(&deep_bytes, (64, 32)).is_err());
        let nv12 = layout("nv12", &[(1, (0, 0)), (2, (1, 1))], SampleBytes::One, 8);
        assert!(SlotShape::of(&nv12, (0, 32)).is_err());
    }
}
