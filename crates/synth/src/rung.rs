//! Rung 1's camera, far tune, frames and views (M3 plan 4.2, amendment
//! 5), kept where every rung reaches them: render's `tests/instrument.rs`
//! (rung 1) takes its camera, tune, frames and views from here, and its
//! scenes, rigs and plants (render's `tests/common/rung1.rs` and
//! `attribution.rs`, which M3.7 lifted from it) add to them; rung 3
//! (M3.5) renders this pair through `render_rig_of` beside the file synth
//! writes of it; and M3.7's rung 2 takes them too. There is one copy: a
//! change here is a change to every rung.
//!
//! The rig the frames are stitched with isn't here: it is sphere's, of
//! [`Camera::cal`] under [`Camera::planted`] through the boundary
//! (`spherecut-boundary`'s `sphere_rig`). synth's own camera
//! ([`render_lens`](crate::lens::render_lens), [`compose`] and
//! [`print`](crate::insv::print)) never calls sphere's lens maths (4.1
//! rule 2), and this module doesn't either.

use std::f64::consts::FRAC_PI_2;
use std::sync::OnceLock;

use glam::DQuat;
use spherecut_core::video::{
    Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, PixelLayout, PlaneData,
    PlaneDesc, Primaries, Range, SampleBytes, Transfer, VideoPlanes,
};
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_sphere::rig::Tune;

use crate::Pattern;
use crate::lens::{
    CalibTruth, Coding, Planted, RenderCache, RigTruth, X5_LIKE_SIDE, compose, lens_planes,
    x5_like, x5_like_planted,
};
use crate::yuv::YuvPlanes;

/// The side of a lens's track (768).
pub const SIDE: u32 = X5_LIKE_SIDE;

/// Both tracks' sides, as `Rig::build` takes them.
pub const SIDES: [u32; 2] = [SIDE; 2];

/// How many samples a side synth takes for each of its pixels (rung 1's:
/// 2).
pub const SS: u32 = 2;

/// The track whose lens faces the front.
pub const FRONT_TRACK: u8 = 0;

/// The side of a rectilinear view of [`whole_view`]'s, in pixels.
pub const VIEW_SIDE: u32 = 256;

/// Rung 1's camera (4.2): synth's X5-like calibration under its planted
/// conventions, composed at [`SIDE`] with track [`FRONT_TRACK`] in
/// front.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub cal: CalibTruth,
    pub planted: Planted,
    /// The camera seeing the scene infinitely far.
    pub truth: RigTruth,
}

/// The camera, made once.
pub fn camera() -> &'static Camera {
    static CAMERA: OnceLock<Camera> = OnceLock::new();
    CAMERA.get_or_init(|| {
        let (cal, planted) = (x5_like(), x5_like_planted());
        Camera {
            truth: compose(&cal, &planted, FRONT_TRACK, SIDES),
            cal,
            planted,
        }
    })
}

/// The default tune stitched infinitely far.
pub fn far() -> Tune {
    Tune {
        q_per_m: 0.0,
        ..Tune::default()
    }
}

/// One track's picture as the X5 codes it: 8-bit 4:2:0 in three planes,
/// full range, BT.709, chroma sited left, each row exactly as long as
/// its plane is wide.
#[derive(Debug, Clone)]
pub struct LensFrame {
    layout: PixelLayout,
    side: u32,
    planes: [Vec<u8>; 3],
    color: ColorInfo,
}

impl LensFrame {
    /// The frame of `planes`, the planes of a `side` × `side` picture.
    ///
    /// # Panics
    ///
    /// If the planes aren't 8-bit, or aren't those of a `side` × `side`
    /// 4:2:0 picture.
    pub fn new(planes: YuvPlanes, side: u32) -> LensFrame {
        let YuvPlanes::U8(planes) = planes else {
            panic!("a lens frame takes 8-bit planes");
        };
        let layout = i420();
        for (i, (desc, bytes)) in layout.planes.iter().zip(&planes).enumerate() {
            let (w, h) = desc.size(side, side);
            assert_eq!(
                bytes.len(),
                w as usize * h as usize,
                "plane {i} of a {side}² 4:2:0 picture is {w}x{h}"
            );
        }
        LensFrame {
            layout,
            side,
            planes,
            color: color(),
        }
    }
}

impl VideoPlanes for LensFrame {
    fn layout(&self) -> &PixelLayout {
        &self.layout
    }

    fn size(&self) -> (u32, u32) {
        (self.side, self.side)
    }

    fn plane(&self, index: usize) -> Option<PlaneData<'_>> {
        let (bytes, desc) = (self.planes.get(index)?, self.layout.planes.get(index)?);
        let (width, height) = desc.size(self.side, self.side);
        Some(PlaneData {
            bytes,
            stride: width as usize,
            width,
            height,
        })
    }

    fn color(&self) -> &ColorInfo {
        &self.color
    }
}

/// FFmpeg's `yuv420p`.
fn i420() -> PixelLayout {
    let luma = PlaneDesc {
        components: 1,
        bytes: SampleBytes::One,
        log2_sub: (0, 0),
    };
    let chroma = PlaneDesc {
        log2_sub: (1, 1),
        ..luma
    };
    PixelLayout {
        name: "yuv420p".into(),
        planes: vec![luma, chroma, chroma],
        family: ColorFamily::Yuv,
        bit_depth: 8,
        msb_aligned: false,
    }
}

/// The colour of [`Coding::X5`]'s planes, with no codes: what render's
/// tests give a frame they make.
fn color() -> ColorInfo {
    ColorInfo {
        codes: ColorCodes::UNSPECIFIED,
        range: Range::Full,
        matrix: Matrix::Bt709,
        transfer: Transfer::Sdr,
        primaries: Primaries::Bt709,
        siting: ChromaSiting::Left,
        assumed: Assumed::default(),
    }
}

/// Track `track`'s frame 0 of `pattern` through the camera, from the
/// process's [`RenderCache::shared`], coded as the X5 codes it.
pub fn track(pattern: Pattern, track: usize) -> LensFrame {
    track_through(pattern, &camera().truth, track)
}

/// Both tracks' frames through the camera, rendered side by side, by
/// track.
///
/// # Panics
///
/// If a track's render panics.
pub fn pair(pattern: Pattern) -> [LensFrame; 2] {
    pair_through(pattern, &camera().truth)
}

/// Track `track`'s frame 0 of `pattern` through `truth`, another camera
/// of rung 1's (the camera at a nearer scene, a lens late or gained, or
/// other lenses), at [`SS`] from the process's [`RenderCache::shared`],
/// coded as the X5 codes it at the lens's own side.
///
/// # Panics
///
/// If `truth` has no lens `track`.
pub fn track_through(pattern: Pattern, truth: &RigTruth, track: usize) -> LensFrame {
    let picture = RenderCache::shared().render(pattern, truth, track, 0, SS);
    LensFrame::new(lens_planes(&picture, Coding::X5), truth.lenses[track].side)
}

/// Both tracks' frames through `truth`, rendered side by side, by track.
///
/// # Panics
///
/// If a track's render panics.
pub fn pair_through(pattern: Pattern, truth: &RigTruth) -> [LensFrame; 2] {
    std::thread::scope(|s| {
        let tracks = [0, 1].map(|t| s.spawn(move || track_through(pattern, truth, t)));
        tracks.map(|t| t.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
    })
}

/// A view of rung 1's, in sphere's terms: the turn from view space to the
/// world, the lens it looks through, `None` for the whole sphere as
/// equirect, and its size in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot {
    pub turn: DQuat,
    pub lens: Option<Lens>,
    pub size: (u32, u32),
}

/// The whole sphere as equirect, unturned, at 512 × 256.
pub fn equirect() -> Shot {
    Shot {
        turn: DQuat::IDENTITY,
        lens: None,
        size: (512, 256),
    }
}

/// 4.2's rectilinear view `k`, 90° at [`VIEW_SIDE`]², on a side seam or
/// a pole of the rig, where the seam's circle crosses +X (`k` = 0), −X (1), +Y (2)
/// and −Y (3).
pub fn whole_view(k: usize) -> Shot {
    let turn = match k {
        0 => DQuat::from_rotation_y(-FRAC_PI_2),
        1 => DQuat::from_rotation_y(FRAC_PI_2),
        2 => DQuat::from_rotation_x(FRAC_PI_2),
        _ => DQuat::from_rotation_x(-FRAC_PI_2),
    };
    Shot {
        turn,
        lens: Some(Lens::new(Projection::Rectilinear, FRAC_PI_2)),
        size: (VIEW_SIDE, VIEW_SIDE),
    }
}
