//! A lens pair as the shader takes it (M3 plan 10.3, 10.4): the rig's
//! uniform, `shaders/rig.wgsl`'s `Lens` and `Rig`, and [`pack`], the only
//! place a [`Rig`]'s f64 numbers become the shader's f32 ones.

use spherecut_sphere::rig::Rig;

use crate::error::RenderError;
use crate::frame::GpuFrame;

/// One calibration block's lens in [`RigUniform`] (`shaders/rig.wgsl`'s
/// `Lens`; a naga test checks every offset). Angles are in radians and
/// lengths in metres; the model's numbers are in its slot's pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LensUniform {
    /// The rows of `lens_from_rig` as a matrix (xyz; w unused).
    pub rot: [[f32; 4]; 3],
    /// `centre_m` (xyz), and θ_max in w: the mask is tested in angle,
    /// not by its cosine.
    pub centre: [f32; 4],
    /// fx, fy, cx and cy, in the model's pixels.
    pub focal: [f32; 4],
    /// k1 to k4: Mei's radial terms in r², or Kannala–Brandt's in θ.
    pub radial: [f32; 4],
    /// k5, ξ (Mei's alone), p1 and p2.
    pub tan: [f32; 4],
    /// p1', p2', s1 and s2.
    pub ext: [f32; 4],
    /// s3, s4, texels per radian at θ_max, and a Kannala–Brandt lens's
    /// base, 0 to 3 in `Lens::radius`'s order (equidistant, equisolid,
    /// stereographic, orthographic).
    pub prism: [f32; 4],
    /// θ_frame, the band's texels per radian, and the slot's side in
    /// texels as `RigLens::side` gives it, which the frame term reads (w
    /// unused).
    pub frame: [f32; 4],
    /// The slot's luma size in texels, which a texel is divided by to
    /// give its uv (zw unused).
    pub slot: [f32; 4],
    /// The frame's gain for this block, linear RGB (w unused).
    pub gain: [f32; 4],
}

/// A lens pair as `fs_rig` reads it, at group 0, binding 1, beside the
/// view (`shaders/rig.wgsl`'s `Rig`; a naga test checks every offset):
/// 416 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RigUniform {
    /// By calibration block.
    pub lens: [LensUniform; 2],
    /// The stitch distance as q = 1/D, in m⁻¹.
    pub q: f32,
    /// The feather's half width h.
    pub feather: f32,
    /// The mask's margin at LOD 0, in texels.
    pub margin: f32,
    /// 0 Mei, 1 Kannala–Brandt: one model for both lenses.
    pub model: u32,
    /// Each block's track. The shader never reads it; the projection's
    /// draw binds block 0's slot at group 1 and block 1's at group 3 by
    /// it.
    pub tracks: [u32; 2],
    pub _pad: [u32; 2],
}

/// What [`pack`] needs of a lens's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotGeom {
    /// The luma plane's size in texels, the picture's.
    pub luma: (u32, u32),
}

impl SlotGeom {
    /// The geometry of the picture `slot` holds.
    pub fn of(slot: &GpuFrame) -> SlotGeom {
        SlotGeom {
            luma: slot.shape().size,
        }
    }
}

/// `rig` with its lenses' slots, `slots` by track, and the frame's gains,
/// `gains` by calibration block, as the shader takes them.
///
/// `Invalid` for a rig that pairs a Mei lens with a Kannala–Brandt one,
/// which the shader can't draw (its model is one per rig, and such a rig
/// renders on the CPU alone; no file's calibration makes one, since its
/// terms are Mei's), for a gain that isn't a finite number of 0 or more,
/// and for a rig whose blocks don't read tracks 0 and 1, one each, as
/// [`crate::reference::render_rig`] refuses them. It never refuses a
/// `RigLens::side` unlike its slot's luma size: the frame term reads the
/// side, and the uv the slot's size, so a test that defeats the frame
/// term by doubling the side draws on the GPU as on the CPU.
#[expect(unused_variables, reason = "M3.7 unit (a) writes the body")]
pub fn pack(
    rig: &Rig,
    slots: [SlotGeom; 2],
    gains: [[f64; 3]; 2],
) -> Result<RigUniform, RenderError> {
    todo!("M3.7 unit (a)")
}
