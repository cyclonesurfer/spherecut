//! The masks and the strips (M3 plan 8.1 and 8.2): which output pixels a
//! weighted metric counts, and the band about the seam unwrapped through
//! each lens alone. Both apply the mask the stitch applies
//! ([`crate::reference::render_rig`]): θ_max less the margin, and 10.3's
//! frame term.

use spherecut_core::video::VideoPlanes;
use spherecut_sphere::rig::Rig;

use super::{StripSpec, Strips};
use crate::error::RenderError;
use crate::project::ViewParams;

/// The mask of a band about the seam (8.1), for a
/// [`crate::compare::Weights`]: a weight for each pixel of a `size`
/// picture of `rig` through `view`, row after row. It is 1 where the
/// pixel's direction lies at least `inner_deg` and at most `outer_deg`
/// off the seam's plane, z = 0 in the rig frame, and a lens covers it;
/// 0 elsewhere. Rung 1's "more than 12° from the seam" (4.2) is the band
/// from 12° to 90°.
#[expect(unused_variables, reason = "M3.4 unit (b) writes the body")]
pub fn band_weights(
    rig: &Rig,
    view: &ViewParams,
    size: (u32, u32),
    inner_deg: f64,
    outer_deg: f64,
) -> Vec<f64> {
    todo!()
}

/// Calibration block `b`'s coverage (8.1), for a
/// [`crate::compare::Weights`]: a weight for each pixel of a `size`
/// picture of `rig` through `view`, row after row. It is 1 where the
/// lens's mask covers the pixel's sample, as
/// [`crate::reference::render_rig`] draws the lens alone
/// ([`crate::reference::RigDebug::Lens0`] and `Lens1`): θ on the
/// parallax-corrected ray within θ_max less the margin at the sample's
/// LOD, and the sample's footprint inside its slot (10.3's frame term).
/// It is 0 elsewhere.
#[expect(unused_variables, reason = "M3.4 unit (b) writes the body")]
pub fn lens_coverage(rig: &Rig, b: usize, view: &ViewParams, size: (u32, u32)) -> Vec<f64> {
    todo!()
}

/// Both lenses' strips (8.2): the band |ε| ≤ `spec.half_band_deg` about
/// the seam, rendered twice, each lens alone along its parallax-corrected
/// ray ([`Rig::ray`]) through the CPU reference's trilinear footprint, in
/// linear light at unit gain. `lenses` are the two tracks' pictures, by
/// track; the strips come back by calibration block. A sample outside its
/// lens's mask is marked invalid, never clamped.
#[expect(unused_variables, reason = "M3.4 unit (b) writes the body")]
pub fn strips(
    rig: &Rig,
    lenses: [&dyn VideoPlanes; 2],
    spec: &StripSpec,
) -> Result<Strips, RenderError> {
    todo!()
}
