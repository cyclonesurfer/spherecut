//! Rigs for the stitching tests (M3 plan 4.2, 8.3): sphere's rig of the
//! camera synth composes, and the frames its lenses' planes make. synth's
//! lens truth and sphere's lens maths share no code (4.1 rule 2), and
//! these tests convert between their types only at the boundary, field
//! by field.
//!
//! Pat's truths and the boundary live in `spherecut-boundary` (the project
//! review's F137), which this module re-exports: [`pats`], parsed from
//! `testkit::pats`, the boundary ([`calibration`], [`geometry`],
//! [`quarter_turn`], [`euler_conv`], [`v6_reading`] and [`lens_model`])
//! and [`sphere_rig`], `Rig::build` through it. They were a copy of those
//! in synth's `tests/lens_against_sphere.rs`, which holds `Rig::build` to
//! synth's `compose` through the same crate now. [`rig_of_truth`] and
//! [`lens_frame`] are render's own.
//!
//! `Tune::default()`'s stitch distance is q = 0.5 m⁻¹ (2 m). A test whose
//! scene is infinitely far sets `q_per_m` to 0, or its lenses sit about
//! 0.9° apart across the seam (b · q, 3.3).

use spherecut_core::video::{ChromaSiting, Matrix, Range};
use spherecut_sphere::rig::{Rig, RigLens, Tune};
use spherecut_synth::lens::RigTruth;
use spherecut_synth::yuv::YuvPlanes;

// Each test binary takes the names it needs, as common/mod.rs's
// dead_code allowance has it.
#[allow(unused_imports)]
pub use spherecut_boundary::{
    calibration, euler_conv, geometry, lens_model, pats, quarter_turn, sphere_rig, v6_reading,
};

use super::{Frame, Kind, color};

// The rigs and their frames.

/// sphere's rig of a truth that no calibration gives, such as
/// `kb_like`'s (4.2's second rig): each of the truth's lenses through
/// `RigLens::new`, with its model through [`lens_model`], its `to_rig`
/// inverted, its centre and its side, calibration block b on track b. Of
/// `tune` it reads each lens's θ_max, the feather and the stitch
/// distance; the truth holds where each lens points and what it shows, so
/// the tune's angles, centre shifts and scales aren't applied. The mask's
/// margin is 2 texels, as `Rig::build` sets it.
pub fn rig_of_truth(truth: &RigTruth, tune: &Tune) -> Rig {
    Rig {
        lenses: std::array::from_fn(|b| {
            let lens = &truth.lenses[b];
            RigLens::new(
                b,
                lens_model(&lens.model),
                lens.to_rig.inverse(),
                lens.centre_m,
                tune.lenses[b].theta_max_deg.to_radians(),
                lens.side,
            )
        }),
        q: tune.q_per_m,
        feather: tune.feather_deg.to_radians(),
        margin_texels: 2.0,
    }
}

/// A lens's planes, as `lens_planes` codes them under `Coding::X5`, as
/// the frame its track's decoder gives: 8-bit I420, `side` × `side`, full
/// range, BT.709, chroma sited left, with no padding after a row.
///
/// # Panics
///
/// If the planes aren't 8-bit, or aren't those of a `side` × `side`
/// 4:2:0 picture.
pub fn lens_frame(planes: &YuvPlanes, side: u32) -> Frame {
    let YuvPlanes::U8(planes) = planes else {
        panic!("lens_frame takes 8-bit planes");
    };
    let layout = Kind::I420.layout();
    let planes = layout
        .planes
        .iter()
        .zip(planes)
        .enumerate()
        .map(|(i, (desc, bytes))| {
            let (w, h) = desc.size(side, side);
            assert_eq!(
                bytes.len(),
                w as usize * h as usize,
                "plane {i} of a {side}² 4:2:0 picture is {w}x{h}"
            );
            (bytes.clone(), w as usize, (w, h))
        })
        .collect();
    Frame {
        layout,
        size: (side, side),
        planes,
        color: color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
    }
}
