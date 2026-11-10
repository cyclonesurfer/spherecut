//! The masks and the strips (M3 plan 8.1 and 8.2): which output pixels a
//! weighted metric counts, and the band about the seam unwrapped through
//! each lens alone. Both apply the mask the stitch applies
//! ([`crate::reference::render_rig`]): θ_max less the margin, and 10.3's
//! frame term.

use spherecut_core::video::VideoPlanes;
use spherecut_sphere::pixel;
use spherecut_sphere::rig::Rig;

use super::{Strip, StripSpec, Strips};
use crate::error::RenderError;
use crate::project::ViewParams;
use crate::reference::{
    Footprint, LensPair, Rays, check_block, check_tracks, in_view, lens_covers, per_pixel_rows,
};

/// The most pixels [`strips()`] unwraps a strip to, 2²⁴: Tier 2's strip
/// is 2880 × 128, and one at an 8K master's own scale across the seam,
/// about 19 px/°, is 6840 × 304. A spec past it is a mistake, refused
/// before it is allocated.
pub const MAX_STRIP_PIXELS: u64 = 1 << 24;

/// The mask of a band about the seam (8.1), for a
/// [`crate::compare::Weights`]: a weight for each pixel of a `size`
/// picture of `rig` through `view`, row after row. It is 1 where the
/// pixel's direction lies at least `inner_deg` and at most `outer_deg`
/// off the seam's plane, z = 0 in the rig frame, and a lens covers it;
/// 0 elsewhere. Rung 1's "more than 12° from the seam" (4.2) is the band
/// from 12° to 90°.
///
/// A lens covers a pixel where [`lens_coverage`] is 1 for it, and the
/// stitch draws every pixel that a lens covers, a covering lens taking
/// what no feather weighs ([`Rig::weights`]): the band from 0° to 90° is
/// the pixels [`crate::reference::render_rig`] draws, on any rig. So a
/// pixel that no lens of `rig` covers weighs nothing, and a metric under
/// this mask can't see a hole that `rig` leaves in its own picture: where
/// the rig under test may cover less than the true one, the mask to take
/// is the true rig's. Bounds that hold no angle (a NaN, or `inner_deg`
/// above `outer_deg`) leave every weight 0, which the weighted metrics
/// refuse.
pub fn band_weights(
    rig: &Rig,
    view: &ViewParams,
    size: (u32, u32),
    inner_deg: f64,
    outer_deg: f64,
) -> Vec<f64> {
    per_pixel_rows(size, |(i, j)| {
        let c = pixel::centre(i, j);
        if !in_view(view, size, c) {
            return 0.0;
        }
        let rays = Rays::at(|x, y| view.dir(size, x, y), c);
        // The angle off the plane z = 0, by atan2, which keeps its
        // precision at the rig's poles.
        let off_seam = rays.d.z.abs().atan2(rays.d.x.hypot(rays.d.y)).to_degrees();
        let in_band = inner_deg <= off_seam && off_seam <= outer_deg;
        weight(in_band && (lens_covers(rig, 0, &rays) || lens_covers(rig, 1, &rays)))
    })
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
///
/// # Panics
///
/// If `b` is neither 0 nor 1.
pub fn lens_coverage(rig: &Rig, b: usize, view: &ViewParams, size: (u32, u32)) -> Vec<f64> {
    check_block(b);
    per_pixel_rows(size, |(i, j)| {
        let c = pixel::centre(i, j);
        let rays = Rays::at(|x, y| view.dir(size, x, y), c);
        weight(in_view(view, size, c) && lens_covers(rig, b, &rays))
    })
}

/// A mask's weight: 1 where it holds, else 0.
fn weight(holds: bool) -> f64 {
    if holds { 1.0 } else { 0.0 }
}

/// Both lenses' strips (8.2): the band |ε| ≤ `spec.half_band_deg` about
/// the seam, rendered twice, each lens alone along its parallax-corrected
/// ray ([`Rig::ray`]) through the CPU reference's trilinear footprint, in
/// linear light at unit gain. `lenses` are the two tracks' pictures, by
/// track; the strips come back by calibration block. A sample outside its
/// lens's mask is marked invalid, never clamped.
///
/// Each strip is [`StripSpec::size`] pixels, pixel (i, j) looking along
/// [`StripSpec::dir`] at (i + ½, j + ½), and samples as
/// [`crate::reference::render_rig`] samples a lens alone: the footprint
/// from the strip pixel's own four ±½ px rays, the mask at its LOD, the
/// taps, the matrix and the EOTF. Nothing after them: no gain, no dim,
/// and no rounding to f16, so the shifts are measured on what the lens
/// shows and not on a target's steps. An invalid sample's `rgb` is 0.
///
/// This builds both pictures' mip chains and unwraps once: a caller with
/// more than one rig to try on one pair of frames, as the fit has (8.4),
/// makes a [`LensPair`] and calls [`strips_of`].
///
/// Refuses what [`crate::reference::render_rig`] refuses of the rig and
/// the planes, and a spec whose scale or band isn't finite and above 0,
/// whose band passes 90°, or whose strip has no pixels or more than
/// [`MAX_STRIP_PIXELS`].
pub fn strips(
    rig: &Rig,
    lenses: [&dyn VideoPlanes; 2],
    spec: &StripSpec,
) -> Result<Strips, RenderError> {
    strips_of(&LensPair::new(lenses)?, rig, spec)
}

/// [`strips()`] of a pair made ready once: the same strips, bit for bit,
/// without the pair's mip chains built again. The rig is this call's, its
/// binding with it, so one pair serves every rig tried on its two frames.
///
/// Refuses what [`strips()`] refuses of the rig and the spec.
pub fn strips_of(pair: &LensPair, rig: &Rig, spec: &StripSpec) -> Result<Strips, RenderError> {
    let (width, height) = strip_size(spec)?;
    check_tracks(rig)?;
    let samples = per_pixel_rows((width, height), |px| {
        let feet = covered(rig, spec, px);
        [0, 1].map(|b| match &feet[b] {
            Some(f) => (pair.light(rig, b, f, false), true),
            None => ([0.0; 3], false),
        })
    });
    Ok(Strips {
        lenses: [0, 1].map(|b| Strip {
            width,
            height,
            rgb: samples.iter().map(|s| s[b].0).collect(),
            valid: samples.iter().map(|s| s[b].1).collect(),
        }),
    })
}

/// Which samples of each lens's strip are valid, by calibration block:
/// [`Strip::valid`] as [`strips()`] gives it for `rig` and `spec`, row
/// after row, with no picture sampled. A strip drawn another way on
/// [`StripSpec::dir`]'s grid, as the shader will draw Measure seam's
/// (10.5, 13.4), takes its samples' validity from here: the mask at each
/// sample's own footprint's LOD, as it is nowhere else worked out.
///
/// Refuses what [`strips()`] refuses of the spec.
pub fn strip_coverage(rig: &Rig, spec: &StripSpec) -> Result<[Vec<bool>; 2], RenderError> {
    let size = strip_size(spec)?;
    let valid = per_pixel_rows(size, |px| covered(rig, spec, px).map(|f| f.is_some()));
    Ok([0, 1].map(|b| valid.iter().map(|v| v[b]).collect()))
}

/// The size of `spec`'s strip, or why it has none to unwrap.
fn strip_size(spec: &StripSpec) -> Result<(u32, u32), RenderError> {
    let finite_above_0 = |v: f64| v.is_finite() && v > 0.0;
    if !finite_above_0(spec.px_per_deg)
        || !finite_above_0(spec.half_band_deg)
        || spec.half_band_deg > 90.0
    {
        return Err(RenderError::Invalid(format!(
            "a strip of {} px/° over ±{}°: the scale and the band are finite and above 0, and \
             the band at most 90°",
            spec.px_per_deg, spec.half_band_deg
        )));
    }
    let (width, height) = spec.size();
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_STRIP_PIXELS {
        return Err(RenderError::Invalid(format!(
            "a strip of {} px/° over ±{}° is {width}x{height}: a strip has 1 to \
             {MAX_STRIP_PIXELS} pixels",
            spec.px_per_deg, spec.half_band_deg
        )));
    }
    Ok((width, height))
}

/// Each block's footprint at pixel `(i, j)` of `spec`'s strip where its
/// mask covers the sample at the footprint's LOD: `None` where the sample
/// is invalid.
fn covered(rig: &Rig, spec: &StripSpec, (i, j): (u32, u32)) -> [Option<Footprint>; 2] {
    let rays = Rays::at(|x, y| spec.dir(x, y), pixel::centre(i, j));
    [0, 1].map(|b| Footprint::of(rig, b, &rays).filter(|f| rig.covers(b, rays.d, f.lod())))
}
