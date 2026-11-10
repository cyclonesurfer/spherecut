//! 3.4's error budget as the tests hold it (M3 plan 3.4, 8.3): the
//! table's scales and rows, the terms a field lands on, and how far a
//! field's bins move. The seam's own tests hold the predicted fields to
//! it, and the attribution matrix the fields the images give.

use glam::DVec3;
use spherecut_render::seam::shifts::TERMS;
use spherecut_render::seam::{BinShift, Signature};
use spherecut_sphere::rig::{Rig, RigLens};
use spherecut_synth::lens::{CalibTruth, X5_LIKE_SIDE};
use spherecut_testkit::control::{Comparison, at_most};

/// The greater of the worst miss so far and one more, and NaN from the
/// first that is NaN on: a miss that is no number (an invalid bin, a term
/// the fit couldn't say) is never passed over for the misses around it.
pub fn worse(worst: f64, miss: f64) -> f64 {
    if worst.is_nan() || miss <= worst {
        worst
    } else {
        miss
    }
}

/// A signature's eight terms, in `signature_of`'s order.
pub fn terms(s: &Signature) -> [f64; TERMS] {
    [s.a0, s.a1, s.b1, s.a2, s.b2, s.c0, s.c1, s.d1]
}

pub const NAMES: [&str; TERMS] = ["a0", "a1", "b1", "a2", "b2", "c0", "c1", "d1"];

/// 3.4's scales at 768²: source pixels a degree, across the seam and
/// along it.
pub const ACROSS_PX: f64 = 3.81;
pub const ALONG_PX: f64 = 6.0;

/// What a principal point a pixel off at 768² moves the seam by, in
/// degrees across and along: half of 3.4's row for cx + 2 px.
pub const CENTRE_ACROSS_DEG: f64 = 0.52 / 2.0;
pub const CENTRE_ALONG_DEG: f64 = 0.33 / 2.0;

/// The terms' places in [`terms`].
pub const A0: usize = 0;
pub const A1: usize = 1;
pub const B1: usize = 2;
pub const C0: usize = 5;
pub const C1: usize = 6;
pub const D1: usize = 7;

/// How far a figure may lie from 3.4's, in pixels at 768²: 15 % of the
/// table's figure, or 0.2 px where that is more. The plan gives its table
/// no tolerance; this one is the slice's.
pub fn slack_px(table_px: f64) -> f64 {
    (0.15 * table_px.abs()).max(0.2)
}

/// A figure against the table's, or the one worked for it, both in
/// pixels at 768² and signed alike: within [`slack_px`] of it. What
/// [`Budget::hold`], [`Budget::hold_corrected`] and
/// [`Budget::hold_worked`] hold, for a control to take.
pub fn held(got_px: f64, figure_px: f64) -> Comparison<f64> {
    at_most((got_px - figure_px).abs(), slack_px(figure_px))
}

/// Whether [`held`] passes: a NaN fits no figure.
fn fits(got_px: f64, figure_px: f64) -> bool {
    held(got_px, figure_px).passes() == Some(true)
}

/// How far a figure under [`slack_px`]'s floor may lie from the one
/// worked for it, in pixels at 768². Under 0.2 px a hold to 0.2 px holds
/// nothing, neither the figure's size nor its sign: a zero fits it.
pub const SMALL_PX: f64 = 0.05;

/// A figure under [`slack_px`]'s floor against the one worked for it,
/// both signed, in pixels at 768²: within [`SMALL_PX`]. A figure of
/// zero, or of the other sign, fails it wherever the worked one is more
/// than [`SMALL_PX`].
pub fn small(got_px: f64, worked_px: f64) -> Comparison<f64> {
    at_most((got_px - worked_px).abs(), SMALL_PX)
}

/// 3.4's rows as a test holds them: each prints as it is held, and the
/// ones that miss wait for the test's end, so that a run reports every
/// row. `what` says where the test's figures come from: "predicted" for
/// `predict_bins`' fields, "measured" for the images'.
pub struct Budget {
    pub what: &'static str,
    pub missed: Vec<String>,
}

impl Budget {
    pub fn of(what: &'static str) -> Budget {
        Budget {
            what,
            missed: Vec::new(),
        }
    }

    pub fn report(&mut self, line: String, fits: bool) {
        eprintln!("3.4 | {line}{}", if fits { "" } else { ": MISSES" });
        if !fits {
            self.missed.push(line);
        }
    }

    /// Holds a figure to the table's, both in pixels at 768².
    pub fn hold(&mut self, row: &str, got_px: f64, table_px: f64) {
        let what = self.what;
        self.report(
            format!("{row}: {got_px:.2} px {what}, {table_px:.2} px in the table"),
            fits(got_px, table_px),
        );
    }

    /// Holds a figure to `worked_px`, 3.4's arithmetic worked again,
    /// where the table's own figure is another thing's: the figure fits
    /// the one and misses the other. `why` says what the table's figure
    /// is ("one lens's", "fx alone"), as the row prints it.
    pub fn hold_corrected(
        &mut self,
        row: &str,
        got_px: f64,
        worked_px: f64,
        table_px: f64,
        why: &str,
    ) {
        let what = self.what;
        self.report(
            format!(
                "{row}: {got_px:.2} px {what}, {worked_px:.2} px worked again, {table_px:.2} px \
                 in the table, which is {why}"
            ),
            fits(got_px, worked_px) && !fits(got_px, table_px),
        );
    }

    /// Holds a signed figure under [`slack_px`]'s floor to the one worked
    /// for it, within [`SMALL_PX`] ([`small`]). A worked figure of
    /// [`SMALL_PX`] or less misses: against it a zero would fit.
    pub fn hold_small(&mut self, row: &str, got_px: f64, worked_px: f64) {
        let fits = worked_px.abs() > SMALL_PX && small(got_px, worked_px).passes() == Some(true);
        let what = self.what;
        self.report(
            format!(
                "{row}: {got_px:+.3} px {what}, {worked_px:+.3} px worked, held to {SMALL_PX} px"
            ),
            fits,
        );
    }

    /// Holds a figure to one 3.4's arithmetic gives and its table doesn't
    /// print.
    pub fn hold_worked(&mut self, row: &str, got_px: f64, worked_px: f64) {
        let what = self.what;
        self.report(
            format!("{row}: {got_px:.2} px {what}, {worked_px:.2} px worked, not in the table"),
            fits(got_px, worked_px),
        );
    }

    /// Holds a field to the terms its row names, by 8.3's rule for the
    /// measured ones: every other term under 0.03°, or 15 % of the
    /// largest named.
    pub fn terms(&mut self, row: &str, s: &Signature, named: &[usize]) {
        let t = terms(s);
        let size = |i: &usize| t[*i].abs();
        let main = named.iter().map(size).fold(0.0, worse);
        let other = (0..TERMS)
            .filter(|i| !named.contains(i))
            .max_by(|a, b| size(a).total_cmp(&size(b)))
            .expect("a term the row doesn't name");
        let names: Vec<&str> = named.iter().map(|&i| NAMES[i]).collect();
        self.report(
            format!(
                "{row}: on {} ({main:.4}°), the largest other term {} ({:.4}°)",
                names.join(", "),
                NAMES[other],
                t[other].abs()
            ),
            main.is_finite() && t[other].abs() <= (0.15 * main).max(0.03),
        );
    }
}

/// How far a field's bins move, in pixels at 768² by 3.4's scales, over
/// its valid bins.
pub struct Sizes {
    /// The least and the most a bin moves.
    pub least: f64,
    pub most: f64,
    /// The most a bin moves along the seam.
    pub along: f64,
}

pub fn sizes(bins: &[BinShift]) -> Sizes {
    let mut sizes = Sizes {
        least: f64::INFINITY,
        most: 0.0,
        along: 0.0,
    };
    for b in bins.iter().filter(|b| b.valid) {
        let (across, along) = (b.across_deg * ACROSS_PX, b.along_deg * ALONG_PX);
        // `min` and `max` would pass a NaN over.
        assert!(across.is_finite() && along.is_finite(), "{b:?}");
        sizes.least = sizes.least.min(across.hypot(along));
        sizes.most = sizes.most.max(across.hypot(along));
        sizes.along = sizes.along.max(along.abs());
    }
    sizes
}

/// A field's first harmonic across the seam, and along it, in degrees.
pub fn harmonics(s: &Signature) -> (f64, f64) {
    (s.a1.hypot(s.b1), s.c1.hypot(s.d1))
}

/// The radius of the seam's circle in a lens's picture, in its slot's
/// pixels: r(90°) from where the lens's axis shows, averaged round the
/// lens.
pub fn seam_radius(lens: &RigLens) -> f64 {
    let at = |d: DVec3| lens.model.project(d).expect("a direction the lens shows");
    let axis = at(DVec3::Z);
    let round = (0..360).map(|deg| {
        let (sin, cos) = f64::from(deg).to_radians().sin_cos();
        (at(DVec3::new(cos, sin, 0.0)) - axis).length()
    });
    round.sum::<f64>() / 360.0
}

/// How far apart a calibration's two principal points lie, each in its
/// own square, in pixels at 768² under map C: block 0's less block 1's.
pub fn centres_apart(cal: &CalibTruth) -> [f64; 2] {
    let src = f64::from(cal.canvas[1]);
    let to_slot = f64::from(X5_LIKE_SIDE) / src;
    let [first, second] = &cal.blocks;
    [
        (first.cx - (second.cx - src)) * to_slot,
        (first.cy - second.cy) * to_slot,
    ]
}

/// `direct` with each track's lens taking, through `part`, what `reversed`
/// has on that track: a part of what reversing the binding swaps.
pub fn swapped(direct: &Rig, reversed: &Rig, part: impl Fn(&mut RigLens, &RigLens)) -> Rig {
    let mut lenses = direct.lenses;
    for lens in &mut lenses {
        let there = reversed.lenses.iter().find(|l| l.track == lens.track);
        part(lens, there.expect("a lens on each track"));
    }
    Rig { lenses, ..*direct }
}
