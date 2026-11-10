//! Measuring the seam of a stitched lens pair (M3 plan section 8): the
//! masks the weighted metrics take ([`band_weights`] and
//! [`lens_coverage`], 8.1), and the seam signature (8.2), which unwraps
//! the band about the seam through each lens alone ([`strips()`]),
//! measures how far one lens's picture sits from the other's in each bin
//! of azimuth ([`bin_shifts`]) and fits the terms a wrong geometry leaves
//! ([`signature`]).
//!
//! In the rig frame (`spherecut_sphere::rig`: +X right, +Y up, the front
//! lens looking along −Z) the seam is the plane z = 0. A strip's point is
//! (φ, ε): φ runs around the seam, 0 on the right (+X), 90° at the zenith
//! and 270° at the nadir, and ε is the angle off the seam towards the
//! front lens, so the point's direction is d = (cos ε cos φ, cos ε sin φ,
//! −sin ε) ([`StripSpec::dir`]).
//!
//! The instrument samples through the CPU reference, never through the
//! shader it measures (4.1 rule 2).

pub mod shifts;
pub mod strips;

use glam::DVec3;

pub use shifts::{bin_shifts, signature};
pub use strips::{band_weights, lens_coverage, strips};

/// How the band about the seam is unwrapped and measured (8.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripSpec {
    /// The strip's scale along the seam and across it, in pixels a
    /// degree.
    pub px_per_deg: f64,
    /// Half the band's height, in degrees: the strip covers |ε| up to it.
    pub half_band_deg: f64,
    /// The width of a bin in φ, in degrees.
    pub bin_deg: f64,
    /// How far either way the coarse search looks for a bin's shift, in
    /// degrees.
    pub search_deg: f64,
}

impl StripSpec {
    /// Tier 1's: the X5-like source's own scale across the seam at 768²
    /// (3.81 px/°, 3.4); a band of ±8°, inside the X5's ±10° overlap with
    /// room for the mask's margin; 72 bins of 5°; and a search of ±3°,
    /// enough for the 2.2° focal control.
    pub const TIER1: StripSpec = StripSpec {
        px_per_deg: 3.81,
        half_band_deg: 8.0,
        bin_deg: 5.0,
        search_deg: 3.0,
    };

    /// Tier 2's, for real footage: the same at 8 px/°.
    pub const TIER2: StripSpec = StripSpec {
        px_per_deg: 8.0,
        ..StripSpec::TIER1
    };

    /// A strip's width and height in pixels: round(360 · `px_per_deg`),
    /// the whole turn of φ, by round(2 · `half_band_deg` · `px_per_deg`).
    /// Rounding leaves the strip's own scale a little off `px_per_deg`
    /// (Tier 1's 1372 × 61 is 3.811 px/° along and 3.8125 across), so a
    /// pixel's angles are [`StripSpec::dir`]'s, from this size.
    pub fn size(&self) -> (u32, u32) {
        (
            (360.0 * self.px_per_deg).round() as u32,
            (2.0 * self.half_band_deg * self.px_per_deg).round() as u32,
        )
    }

    /// The rig-frame unit direction at the strip's point `(x, y)`, in
    /// pixels from the top-left corner (pixel (i, j)'s centre is at
    /// (i + ½, j + ½)): φ = 360° · x/width, ε = `half_band_deg` −
    /// 2 · `half_band_deg` · y/height, and d = (cos ε cos φ, cos ε sin φ,
    /// −sin ε). So x runs with φ, and the top row is the front lens's
    /// side of the seam.
    pub fn dir(&self, x: f64, y: f64) -> DVec3 {
        let (width, height) = self.size();
        let phi = (360.0 * x / f64::from(width)).to_radians();
        let eps =
            (self.half_band_deg - 2.0 * self.half_band_deg * y / f64::from(height)).to_radians();
        let (sin_phi, cos_phi) = phi.sin_cos();
        let (sin_eps, cos_eps) = eps.sin_cos();
        DVec3::new(cos_eps * cos_phi, cos_eps * sin_phi, -sin_eps)
    }
}

/// One lens's unwrapped band about the seam ([`strips()`]): `width` ×
/// `height` pixels of a [`StripSpec`], row after row from the top, the
/// front lens's side.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    pub width: u32,
    pub height: u32,
    /// Each pixel's R, G and B in linear light at unit gain: the frame's
    /// gains aren't applied, so that the signature's photometric part
    /// measures what they should be.
    pub rgb: Vec<[f64; 3]>,
    /// Whether the lens covers each pixel's sample. Outside the lens's
    /// mask (θ_max less the margin, and 10.3's frame term) a sample is
    /// invalid, and its `rgb` is nothing to read: never a clamped texel.
    pub valid: Vec<bool>,
}

/// Both lenses' strips of one frame, by calibration block, as a rig's
/// lenses go.
#[derive(Debug, Clone, PartialEq)]
pub struct Strips {
    pub lenses: [Strip; 2],
}

/// How far lens 1's strip sits from lens 0's in one bin of φ (8.2), the
/// lenses by calibration block.
///
/// The shift is the one that carries lens 0's strip onto lens 1's: what
/// lens 0's strip shows at (φ, ε), lens 1's shows at (φ + `along_deg`,
/// ε + `across_deg`). So across is positive towards +ε, the front lens
/// (up the strip), and along is positive towards +φ (to the right along
/// it). Swapping the strips flips both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BinShift {
    /// The bin's centre in φ, in degrees.
    pub phi_deg: f64,
    /// The shift across the seam, in degrees of ε.
    pub across_deg: f64,
    /// The shift along the seam, in degrees of φ.
    pub along_deg: f64,
    /// The shift's information: the 2 × 2 normal matrix of the bin's
    /// Lucas–Kanade steps, its rows and columns in the order across,
    /// along. Texture along a single line gives information across the
    /// line and none along it (the aperture problem), so a fit weighs
    /// each bin's shift by this rather than taking a false shift.
    pub info: [[f64; 2]; 2],
    /// The normalised cross-correlation of the two lenses' patches at the
    /// shift, from −1 to 1.
    pub ncc: f64,
    /// The photometric part: the median over the bin's samples of the
    /// natural log of lens 1's value over lens 0's, for R, G and B.
    /// Samples above 0.95 or below 0.01 are left out.
    pub log_gain: [f64; 3],
    /// Whether the bin measured a shift. A bin with no texture to measure
    /// one from is invalid, and its shift is no measurement: never a
    /// zero.
    pub valid: bool,
}

/// The seam signature of one frame (8.2): the least-squares fit to the
/// valid bins' shifts, weighted by each bin's information, of
///
/// ```text
/// across(φ) = a0 + a1 cos φ + b1 sin φ + a2 cos 2φ + b2 sin 2φ
/// along(φ)  = c0 + c1 cos φ + d1 sin φ
/// ```
///
/// with what the fit leaves. The terms are in degrees and take
/// [`BinShift`]'s signs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Signature {
    /// Across, uniform: a focal scale, parallax at a uniform depth, or a
    /// distortion modelled wrong in θ.
    pub a0: f64,
    /// Across, first harmonic, with `b1`: a relative tilt, a centre
    /// offset, the Euler class, the binding reversed, a lens late under a
    /// pan or tilt.
    pub a1: f64,
    pub b1: f64,
    /// Across, second harmonic, with `b2`: fx ≠ fy.
    pub a2: f64,
    pub b2: f64,
    /// Along, uniform: a relative roll.
    pub c0: f64,
    /// Along, first harmonic, with `d1`: the centres' difference, and
    /// V6's tangential and prism terms.
    pub c1: f64,
    pub d1: f64,
    /// The rms of what the fit leaves of the valid bins' shifts, in
    /// degrees: depth that varies with azimuth, and the part of a rolling
    /// shutter that isn't rigid.
    pub rest_rms_deg: f64,
    /// How many bins measured a shift.
    pub valid_bins: usize,
    /// The median of the valid bins' NCC. Below 0.5 the stitch has failed
    /// grossly: the turn wrong or left out, or a gross Euler error. A
    /// texture that repeats inside the search can keep it high all the
    /// same: on synth's `SeamLines`, with the turn left out, the bins
    /// lock a period of its checker away at 0.97
    /// (`tests/instrument.rs`).
    pub median_ncc: f64,
    /// The photometric log gains of lens 1 over lens 0, for R, G and B,
    /// over the valid bins ([`BinShift::log_gain`]): exposure and white
    /// balance.
    pub log_gain: [f64; 3],
}
