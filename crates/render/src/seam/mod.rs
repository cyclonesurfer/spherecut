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
pub use strips::{band_weights, lens_coverage, strip_coverage, strips, strips_of};

/// How the band about the seam is unwrapped and measured (8.2).
///
/// A spec has one scale, so [`StripSpec::size`], [`StripSpec::dir`] and
/// [`strip_coverage`] make strips of one scale along the seam and across
/// it: 1372 × 61 at Tier 1 and 2880 × 128 at Tier 2. The 1440 × 128
/// target that 13.4 gives Measure seam is neither, at 4 px a degree along
/// and 8 across. [`bin_shifts`] reads a strip's own two scales and would
/// take such a strip; nothing here makes one.
///
/// No view draws this grid. An equirect [`crate::ViewParams`] fills its
/// target with the whole sphere and takes no framing, and a strip is no
/// crop of an equirect at its scale: Tier 1's rows are 16°/61 each with
/// the middle row's centre on the seam, where an equirect 1372 wide has
/// rows of 180°/686 and a boundary between two of them. So a shader that
/// draws a strip (M3.7) needs an output whose pixel (i, j) looks along
/// [`StripSpec::dir`] at (i + ½, j + ½) in the rig frame, and takes the
/// strip's `valid` from [`strip_coverage`].
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
///
/// Both are in degrees at either tier. Nothing here converts a shift to
/// source pixels, as 8.2 has Tier 2's reported: what reports one does it
/// by the source's own two scales at the seam, which differ (at 768² a
/// lens, 3.81 px a degree across the seam and 6.0 along it, 3.4).
///
/// What reads the bins weights each by `info`, and should gate each on
/// its own `ncc` as well. `valid` says only that the bin found a match
/// and settled on it, not that the two patches show one thing: where a
/// lens shows another scene, or something moves or hides in the overlap,
/// a bin comes back valid with a shift of several pixels at an NCC near
/// 0, and where its texture is fine enough it has information too.
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
    ///
    /// Its unit is degrees⁻²: the sum over the patch's samples of the
    /// squared slope, per degree of shift, of lens 1's patch after its
    /// gain and bias are normalised out (the patch less its mean, over
    /// its standard deviation). So it holds the texture's shape and the
    /// number of samples, and leaves out the patch's contrast and the
    /// noise's scale: a patch at a fiftieth of the contrast has the same
    /// information and, under the same noise, fifty times the error.
    /// `contrast` is there to weight or gate by; and in these units a
    /// bin's own mismatch is 2 (1 − `ncc`), the mean square difference of
    /// its two normalised patches.
    ///
    /// All zero for an invalid bin, and for a valid one whose texture
    /// doesn't stand clear of its mismatch ([`shifts::NOISE_FLOOR`]: a
    /// flat patch or a ramp under noise), which measures only its noise.
    /// Where the texture stands clear one way only (a horizon across a
    /// noisy sky), the information is that way's alone, as a line's. The
    /// same holds of the patch's interior by itself, the patch less the
    /// three samples at each edge that the smoothing makes partly of
    /// what lies past it: a sky bin beside texture has none
    /// ([`bin_shifts`]).
    ///
    /// A bin that is part sky and part texture has it, and more than its
    /// shift is worth: the deviation it is normalised by is spread over
    /// the sky's samples too. With texture over a fifth of its columns a
    /// bin has 0.85 to 1.7 times the information of one textured
    /// throughout, and measures up to three times worse.
    pub info: [[f64; 2]; 2],
    /// The normalised cross-correlation of the two lenses' patches at the
    /// shift, from −1 to 1.
    pub ncc: f64,
    /// Each lens's contrast over the patch, lens 0's and lens 1's: the
    /// standard deviation of its luma over the mean, over the samples
    /// the shift is measured from, lens 0's at its pixels and lens 1's
    /// where the shift puts them, both off the smoothed strips, which
    /// keep less of a fine texture's contrast than the strips have. NaN
    /// for an invalid bin.
    pub contrast: [f64; 2],
    /// The photometric part: the median over the bin's samples of the
    /// natural log of lens 1's value over lens 0's, for R, G and B.
    /// Samples above 0.95 or below 0.01 are left out. It is read at the
    /// bin's shift, so where the bin has no information it is as good as
    /// the patch is flat.
    pub log_gain: [f64; 3],
    /// Whether the bin measured a shift: it found a match and settled on
    /// it. A bin with no texture to measure one from is invalid, and its
    /// shift is no measurement: never a zero. A valid bin's shift may
    /// still be its noise's alone, or a match on texture that lies past
    /// its patch's edge, with no information ([`Self::informed`]).
    pub valid: bool,
}

impl BinShift {
    /// Whether the bin's shift says anything of the seam: the bin is
    /// valid and has information. [`signature`] fits to these bins alone.
    pub fn informed(&self) -> bool {
        self.valid && self.info[0][0] + self.info[1][1] > 0.0
    }
}

/// The seam signature of one frame (8.2): the least-squares fit to the
/// shifts of the bins with information ([`BinShift::informed`]),
/// weighted by each bin's information, of
///
/// ```text
/// across(φ) = a0 + a1 cos φ + b1 sin φ + a2 cos 2φ + b2 sin 2φ
/// along(φ)  = c0 + c1 cos φ + d1 sin φ
/// ```
///
/// with what the fit leaves. The terms are in degrees and take
/// [`BinShift`]'s signs.
///
/// A term is NaN only where the bins leave the fit short of rank for it
/// (to 10⁻¹⁰). Bins on one side of the ring leave the terms finite and
/// ill-conditioned: one 90° arc of 18 bins puts a0 and a1 at 47 times a
/// bin's noise, two opposite sides of 12 bins at 1.6 times at most, the
/// whole ring at 0.12 to 0.17. `valid_bins` and `informed_bins` don't
/// tell these apart; `term_se_deg` does ([`signature`] has the detail).
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
    /// Each term's standard error, in degrees, in the order a0, a1, b1,
    /// a2, b2, c0, c1, d1: from the normal matrix's inverse and what the
    /// fit leaves over its degrees of freedom, whatever the scale of the
    /// bins' information. NaN where the term is, and where the bins
    /// leave nothing over to tell the noise from. It covers the bins'
    /// noise, not a bias they share, and takes each bin's information
    /// for what its shift is worth, which a bin of fainter texture than
    /// the rest, or one part sky, has too much of ([`BinShift::info`]).
    pub term_se_deg: [f64; shifts::TERMS],
    /// The rms of what the fit leaves of the fitted bins' shifts, in
    /// degrees: depth that varies with azimuth, and the part of a rolling
    /// shutter that isn't rigid.
    pub rest_rms_deg: f64,
    /// How many bins measured a shift.
    pub valid_bins: usize,
    /// How many of those had information: the bins the terms are fitted
    /// to. The rest measured only their noise, or what lies past their
    /// patches' edges.
    pub informed_bins: usize,
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

#[cfg(test)]
mod tests {
    use glam::DVec3;
    use spherecut_testkit::control::{at_most, must_fail};

    use super::StripSpec;

    /// [`StripSpec::dir`] is 8.2's, held by directions written out here
    /// and through nothing else of the instrument: φ is 0 on the right
    /// (+X), 90° at the zenith (+Y), 180° on the left (−X) and 270° at the
    /// nadir (−Y), and ε = +8°, the strip's top edge, leans towards the
    /// front lens, which looks along −Z.
    #[test]
    fn dir_is_8_2s_by_literals() {
        const WITHIN: f64 = 1e-12;
        let spec = StripSpec::TIER1;
        assert_eq!(spec.size(), (1372, 61));
        let (sin_8, cos_8) = 8f64.to_radians().sin_cos();
        // The strip's point (x, y), and the direction there.
        let points = [
            // The seam itself, ε = 0: the row y = 30.5.
            ((0.0, 30.5), DVec3::new(1.0, 0.0, 0.0)),
            ((343.0, 30.5), DVec3::new(0.0, 1.0, 0.0)),
            ((686.0, 30.5), DVec3::new(-1.0, 0.0, 0.0)),
            ((1029.0, 30.5), DVec3::new(0.0, -1.0, 0.0)),
            // ε = +8° at the top edge, towards the front lens (−Z), and
            // −8° at the bottom.
            ((0.0, 0.0), DVec3::new(cos_8, 0.0, -sin_8)),
            ((343.0, 0.0), DVec3::new(0.0, cos_8, -sin_8)),
            ((686.0, 61.0), DVec3::new(-cos_8, 0.0, sin_8)),
            ((1029.0, 61.0), DVec3::new(0.0, -cos_8, sin_8)),
        ];
        let held = |dir: &dyn Fn(f64, f64) -> DVec3| {
            let worst = points
                .iter()
                .map(|&((x, y), want)| (dir(x, y) - want).length())
                .fold(0.0, f64::max);
            at_most(worst, WITHIN)
        };
        let dir = held(&|x, y| spec.dir(x, y));
        assert!(dir.passes() == Some(true), "{dir}");

        // Controls: φ reflected puts the nadir at 90°; φ a half turn on
        // puts the left at 0°; ε reflected leans the top edge to the back
        // lens.
        let off = must_fail("φ reflected", held(&|x, y| spec.dir(1372.0 - x, y)));
        assert!((off - 2.0).abs() < WITHIN, "{off}");
        let off = must_fail("φ a half turn on", held(&|x, y| spec.dir(x + 686.0, y)));
        assert!((off - 2.0).abs() < WITHIN, "{off}");
        let off = must_fail("ε reflected", held(&|x, y| spec.dir(x, 61.0 - y)));
        assert!((off - 2.0 * sin_8).abs() < WITHIN, "{off}");
    }
}
