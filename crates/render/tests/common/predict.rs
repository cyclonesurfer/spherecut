//! The seam shifts a model predicts (M3 plan 8.3, step 1), for the
//! attribution matrix and the seam instrument's tests to hold the
//! images' shifts to.
//!
//! A strip made through the model `rig` shows, at its point (x, y) on
//! block b's strip, whatever the truth's picture holds at the model's
//! texel for that point's direction: the scene direction A_b(x, y) =
//! seen(texel_b(dir(x, y))). Where the model is the truth, both strips
//! show the same direction at the same point. Where it isn't, block 1's
//! strip shows at (x + u, y + v) what block 0's shows at (x, y), and
//! (u, v) is the shift `bin_shifts` measures. It is found here by solving
//! A_1(x + u, y + v) = A_0(x, y), with no images.

use glam::{DVec2, DVec3};
use spherecut_render::seam::shifts::{bin_count, patch};
use spherecut_render::seam::{BinShift, StripSpec};
use spherecut_sphere::rig::Rig;
use spherecut_synth::lens::{self, RigTruth};

/// The shift field that unwrapping `truth`'s frame `frame` with the model
/// `rig` leaves, bin by bin as `bin_shifts` gives it under `spec`, with
/// `BinShift`'s signs: point by point in f64, each seam sample through
/// the model's forward and the truth's inverse (`synth::lens::seen`),
/// with no images.
///
/// A bin's shift is the mean over a lattice of points across its patch
/// (`seam::shifts::patch`), the samples `bin_shifts` measures it from,
/// and its `phi_deg` the middle of the patch's columns, as `bin_shifts`
/// gives it. Its information is the identity, its NCC 1 and its log
/// gains 0: a prediction is as sure across the seam as along it.
///
/// A bin is predicted only where the estimator can measure the whole of
/// its patch: each point of the lattice that the truth shows has its
/// shift inside the estimator's coarse search on each axis, `search_deg`
/// of the strip's scale rounded up to whole pixels (±12 px at Tier 1).
/// `bin_shifts` gives one shift for a whole patch, searched for over
/// those whole pixels on each axis, and nothing in it leaves a part of a
/// patch out. So a mean over the points inside the search alone is not
/// what it measures, and a bin with a point past the search is invalid,
/// its shift NaN, whatever its other points say. The mean is no test
/// either: with the tracks swapped on Tier 1's rig bin 6's mean lies
/// 11.8 px across, inside the search, while its farthest points lie
/// 12.2 px, and the estimator gives an alias 5.4 px away, handed on from
/// the bins past the search beside it. The 44 bins whose every point
/// lies within the 12 px come back within 0.08 px of their means. Nor is
/// the shift's length the test, nor 3° on each axis: either leaves out
/// four more bins there (7, 28, 43 and 64), which the estimator measures
/// within 0.03 px.
///
/// A point where the truth shows nothing on either lens is no sample of
/// the estimator's and is left out of the mean; a bin is invalid where
/// they are more than half its points, and where a point's solve doesn't
/// settle. The seam's own tests hold these rules on a field known in
/// closed form
/// (`a_bin_is_predicted_where_its_whole_patch_is_inside_the_search`).
pub fn predict_bins(truth: &RigTruth, frame: u32, rig: &Rig, spec: &StripSpec) -> Vec<BinShift> {
    predict_through(rig, spec, |b, px| {
        lens::seen(truth, rig.lenses[b].track, frame, px)
    })
}

/// [`predict_bins`] with the truth as a function: `shows(b, px)` is the
/// scene direction block b's picture holds at its pixel `px`, or `None`
/// where it holds nothing. The model's own inverse for it, `rig.unproject`
/// of a rig at q = 0, is the control that predicts no shift whatever is
/// planted.
pub fn predict_through(
    rig: &Rig,
    spec: &StripSpec,
    shows: impl Fn(usize, DVec2) -> Option<DVec3>,
) -> Vec<BinShift> {
    let (width, height) = spec.size();
    let sx = f64::from(width) / 360.0;
    let sy = f64::from(height) / (2.0 * spec.half_band_deg);
    let scene = |b: usize, at: DVec2| shows(b, rig.texel(b, spec.dir(at.x, at.y))?);
    // The estimator's coarse search: whole pixels each way, on each axis.
    let search = DVec2::new((spec.search_deg * sx).ceil(), (spec.search_deg * sy).ceil());
    (0..bin_count(spec))
        .map(|bin| {
            let patch = patch(spec, bin);
            // Pixel i covers i to i + 1.
            let (x0, x1) = (f64::from(patch.columns.start), f64::from(patch.columns.end));
            let (y0, y1) = (f64::from(patch.rows.start), f64::from(patch.rows.end));
            let centre = DVec2::new(0.5 * (x0 + x1), 0.5 * (y0 + y1));
            let mean = Slope::at(|at| scene(1, at), centre).and_then(|slope| {
                let mut sum = DVec2::ZERO;
                let (mut found, mut whole) = (0, true);
                for j in 0..LATTICE[1] {
                    for i in 0..LATTICE[0] {
                        let at = DVec2::new(
                            x0 + (f64::from(i) + 0.5) / f64::from(LATTICE[0]) * (x1 - x0),
                            y0 + (f64::from(j) + 0.5) / f64::from(LATTICE[1]) * (y1 - y0),
                        );
                        match carried(scene, &slope, at, search) {
                            Point::Carried(shift) => {
                                sum += shift;
                                found += 1;
                            }
                            Point::Nothing => {}
                            Point::Unheld => whole = false,
                        }
                    }
                }
                (whole && 2 * found >= LATTICE[0] * LATTICE[1]).then(|| sum / f64::from(found))
            });
            let phi_deg = centre.x / sx;
            match mean {
                // The strip's y runs down, against ε.
                Some(shift) => BinShift {
                    phi_deg,
                    across_deg: -shift.y / sy,
                    along_deg: shift.x / sx,
                    info: [[1.0, 0.0], [0.0, 1.0]],
                    ncc: 1.0,
                    contrast: [f64::NAN; 2],
                    log_gain: [0.0; 3],
                    valid: true,
                },
                None => BinShift {
                    phi_deg,
                    across_deg: f64::NAN,
                    along_deg: f64::NAN,
                    info: [[0.0; 2]; 2],
                    ncc: f64::NAN,
                    contrast: [f64::NAN; 2],
                    log_gain: [f64::NAN; 3],
                    valid: false,
                },
            }
        })
        .collect()
}

/// The points a bin's shift is the mean over: columns by rows, each at
/// the middle of its cell of the patch.
pub const LATTICE: [u32; 2] = [5, 7];

/// The solve ends when a step is shorter than this, in strip pixels.
const SETTLED: f64 = 1e-9;
/// The most steps a point takes. A truth whose rows are exposed one
/// after another (a rolling shutter while the rig turns) steps from row
/// to row, so the solve can end with steps of the size of that jump:
/// under [`ROW_JUMP`] they still count.
const STEPS: u32 = 12;
const ROW_JUMP: f64 = 0.01;

/// How block 1's scene direction changes with its strip point: the
/// direction's slope along x and along y, per strip pixel, at a bin's
/// middle.
struct Slope {
    x: DVec3,
    y: DVec3,
}

impl Slope {
    /// By central differences half a pixel either way.
    fn at(scene: impl Fn(DVec2) -> Option<DVec3>, centre: DVec2) -> Option<Slope> {
        let along = |step: DVec2| Some(scene(centre + step)? - scene(centre - step)?);
        Some(Slope {
            x: along(DVec2::new(0.5, 0.0))?,
            y: along(DVec2::new(0.0, 0.5))?,
        })
    }

    /// The step in the strip point that moves the direction by `miss`,
    /// as nearly as a step in the strip can: the least squares of the
    /// two slopes.
    fn step(&self, miss: DVec3) -> Option<DVec2> {
        let (xx, xy, yy) = (self.x.dot(self.x), self.x.dot(self.y), self.y.dot(self.y));
        let det = xx * yy - xy * xy;
        let (mx, my) = (self.x.dot(miss), self.y.dot(miss));
        (det > 0.0).then(|| DVec2::new(yy * mx - xy * my, xx * my - xy * mx) / det)
    }
}

/// What one point of a patch says of its bin's shift.
enum Point {
    /// The shift (u, v) that carries block 0's strip onto block 1's
    /// there, inside the search.
    Carried(DVec2),
    /// The truth shows nothing there on one of the lenses.
    Nothing,
    /// The estimator can't hold the point: its shift lies past the
    /// search on an axis, or the solve for it doesn't settle.
    Unheld,
}

/// The shift that carries block 0's strip at `at` onto block 1's: (u, v)
/// with block 1 showing at `at` + (u, v) the direction block 0 shows at
/// `at`, where it lies within `search` on each axis. The steps may pass
/// the search by a pixel on the way, as the estimator's own may, and no
/// further.
fn carried(
    scene: impl Fn(usize, DVec2) -> Option<DVec3>,
    slope: &Slope,
    at: DVec2,
    search: DVec2,
) -> Point {
    let Some(wanted) = scene(0, at) else {
        return Point::Nothing;
    };
    let past = |shift: DVec2, by: f64| shift.abs().cmpgt(search + by).any();
    let mut shift = DVec2::ZERO;
    let mut last = f64::INFINITY;
    for _ in 0..STEPS {
        let Some(step) = scene(1, at + shift).and_then(|shown| slope.step(wanted - shown)) else {
            return Point::Nothing;
        };
        shift += step;
        last = step.length();
        if past(shift, 1.0) {
            return Point::Unheld;
        }
        if last < SETTLED {
            break;
        }
    }
    if last < ROW_JUMP && !past(shift, 0.0) {
        Point::Carried(shift)
    } else {
        Point::Unheld
    }
}
