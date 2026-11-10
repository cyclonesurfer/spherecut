//! The shifts between the two lenses' strips, bin by bin, and the
//! signature fitted to them (M3 plan 8.2).

use super::{BinShift, Signature, StripSpec, Strips};

/// The shift between the two strips in each bin of `spec.bin_deg` in φ
/// (72 bins of 5°), in order from φ = 0, with [`BinShift`]'s signs. Each
/// bin finds a coarse shift by NCC over ±`spec.search_deg`, refines it
/// with three Lucas–Kanade steps on patches normalised for gain and bias,
/// and keeps the steps' 2 × 2 normal matrix as the shift's information.
/// A bin without texture is marked invalid, never given a zero shift.
#[expect(unused_variables, reason = "M3.4 unit (c) writes the body")]
pub fn bin_shifts(strips: &Strips, spec: &StripSpec) -> Vec<BinShift> {
    todo!()
}

/// The signature of one frame's bins (8.2): the fit of across(φ) and
/// along(φ) to the valid bins' shifts, each weighted by its information,
/// with the rms of what is left, the number of valid bins, their median
/// NCC and the photometric log gains.
#[expect(unused_variables, reason = "M3.4 unit (c) writes the body")]
pub fn signature(bins: &[BinShift]) -> Signature {
    todo!()
}
