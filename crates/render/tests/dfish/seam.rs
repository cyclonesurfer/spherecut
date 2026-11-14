//! The seam's dual-fisheye goldens (M3 plan 10.5), a lens pair stitched
//! by the shader across its seam: both seams of the far `SeamLines` pair,
//! the registration the GPU's strips measure, the stitch distance, the
//! exposure match, the feather, the X5's own coding and the layouts.
//! Rows of the goldens binary, blessed into `goldens/dfish_seam/` with a
//! manifest of their own (see the binary's doc).
//!
//! Every row renders rung 1's X5-like camera at 768² a lens
//! (`common::rung1`, `synth::rung`) from the pairs the binary renders
//! once, and records each pair as 10.5 asks through the run's pair API.
//! The seam rows measure on the GPU's strips (`StillRenderer::strips`)
//! and read them as 8.3 does, through the lifted attribution
//! (`common::attribution`): what a rig predicts with no images
//! (`predict_bins`), and step 3's `attribute_to`. The one pair not at
//! 768² is the V6 camera's, whose wrong reading is a control of the
//! registration: it is rendered at 384², its rigs built at 384², where
//! step 3 attributes the reading as the matrix does at 768² for a quarter
//! of the render. The exposure match's and the feather's lens 1, brighter
//! than the pair's, is the pair's own picture with the gain put on it
//! ([`lens_1_at`]), not a render of its own.

use std::sync::OnceLock;

use glam::DVec3;
use serde_json::{Value, json};
use spherecut_core::video::{
    ChromaSiting, ColorInfo, Matrix, PixelLayout, PlaneData, Range, VideoPlanes,
};
use spherecut_render::reference::{ImageF64, RigDebug, srgb_oetf};
use spherecut_render::seam::{BinShift, Strips, bin_shifts, signature, strips_of};
use spherecut_render::{DebugMode, Image, LensFrame, PlaneSource, StillOptions};
use spherecut_sphere::rig::{Rig, Tune};
use spherecut_synth::lens::{
    Coding, PairOrder, Planted, PrismAt, PrismOrder, RenderCache, RigTruth, compose, lens_planes,
    x5_like_v6,
};
use spherecut_synth::rung::{self, FRONT_TRACK, SS, VIEW_SIDE, far, pair_through};
use spherecut_synth::{Pattern, RgbImage};

use super::common::attribution::{
    Lands, MISREGISTRATION_PX, NCC, Plant, SPEC, Seam, TERM_DEG, apart_deg, attribute_to, gained,
    largest_term, line, reading, rendered_reading, scales,
};
use super::common::budget::{A0, A1, B1, C0, C1, D1, NAMES, terms, worse};
use super::common::predict::predict_bins;
use super::common::rig::sphere_rig;
use super::common::rung1::{
    NEAR_M, Pair, STRIPE, Scene, Shot, UNIT, VIEW_KEYS, VIEWS, both, equirect, ideal_view, lines,
    near, near_lines, ready, seam_runs_down, stitch, tuned, view, whole_view, x5,
};
use super::common::{Frame, Kind, color, frame};
use super::{
    CPU_FLOOR_DB, CPU_MARGIN_DB, CPU_MAX_CHANNEL, IDEAL_MARGIN_DB, Mode, PairSource, Row, Run,
    Stitched, UPDATE, bless, diff_dir_beside, eotf, gpu, max_diff, num, oetf, pair_geometry,
    psnr_in, psnr_of, run_these, sides, unit, view_about,
};

/// Where these rows' images and manifest live.
pub(super) const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/dfish_seam");

/// What their manifest says it is.
pub(super) const ABOUT: &str = "The seam's dual-fisheye goldens (M3 plan 10.5, DECISIONS D32): \
                                lens pairs rendered from synth's planes and stitched by the \
                                shader on lavapipe, with each check's thresholds and what it \
                                measured when blessed.";

/// The rows, in 10.5's order.
pub(super) const ROWS: &[(&str, Row)] = &[
    ("dfish_rect_seam_left", dfish_rect_seam_left),
    ("dfish_rect_seam_right", dfish_rect_seam_right),
    ("dfish_seam_registration", dfish_seam_registration),
    ("dfish_stitch_distance", dfish_stitch_distance),
    ("dfish_exposure_match", dfish_exposure_match),
    ("dfish_feather", dfish_feather),
    ("dfish_fullrange_left", dfish_fullrange_left),
    ("dfish_layout_invariance", dfish_layout_invariance),
];

#[test]
fn dfish_seam_rows() {
    let gpu = gpu();
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let diffs = diff_dir_beside("dfish-seam");
    let _ = std::fs::remove_dir_all(&diffs);
    let mode = Mode::Golden {
        bless: bless::on(UPDATE),
    };
    let mut run = Run::at(gpu, mode, std::path::Path::new(DIR), &diffs);
    run.about = ABOUT;
    run.lower_floors = bless::lower(UPDATE);
    run.tell = true;
    run_these(&mut run, ROWS, None);
    run.finish();
}

// The pairs, and what a row holds them to.

/// The X5-like camera's pair `frames` (by track) of the scene `name`,
/// seen through `tune` with the scene R = `r_m` away (∞ for a far
/// scene), with the frame's `gains`.
fn x5_source<'a>(
    name: &str,
    frames: [&'a dyn VideoPlanes; 2],
    tune: &Tune,
    r_m: f64,
    gains: [[f64; 3]; 2],
) -> PairSource<'a> {
    let x5 = x5();
    let geometry = pair_geometry(&x5.planted, tune, r_m);
    let rig = sphere_rig(&x5.cal, &x5.planted, tune, FRONT_TRACK, sides(frames)).unwrap();
    PairSource::new(name, frames, &x5.cal, geometry, &rig, gains)
}

/// One of rung 1's pairs as the planes a source records.
fn planes(pair: Pair) -> [&'static dyn VideoPlanes; 2] {
    [pair[0], pair[1]]
}

/// Rung 1's far `SeamLines` pair at unit gains, which most rows render.
fn lines_source() -> PairSource<'static> {
    let name = Scene::Lines.row();
    x5_source(name, planes(both(lines())), &far(), f64::INFINITY, UNIT)
}

/// `pair` stitched by `rig` through `shot` in `debug`'s mode, at the
/// pair's own gains, or `None` with the failure recorded.
fn render(
    run: &mut Run<'_>,
    pair: &PairSource<'_>,
    rig: &Rig,
    shot: &Shot,
    debug: DebugMode,
) -> Option<Stitched> {
    let opts = StillOptions {
        debug,
        ..StillOptions::default()
    };
    run.render_pair(pair, rig, pair.gains, &shot.0, shot.1, opts)
}

/// `v` strictly below `bound`, as [`Run::over`] holds a value strictly
/// above one: 10.5's "step < 1 LSB" and "under 1 LSB".
fn under(run: &mut Run<'_>, key: &str, v: f64, bound: f64) -> bool {
    run.measure(key, v);
    run.threshold(&format!("{key}_under"), num(bound));
    let ok = v < bound;
    if !ok {
        run.fail(format!("{key} is {v:.4}, not under {bound}"));
    }
    ok
}

/// A flag held to be set: a check whose outcome is no number.
fn held(run: &mut Run<'_>, key: &str, ok: bool) -> bool {
    run.at_least(key, f64::from(u8::from(ok)), 1.0)
}

/// The floor `key` is held to this run, as [`Run::calibrated`] put it
/// among the row's thresholds: blessed, or calibrated by this blessing.
fn floor_of(run: &Run<'_>, key: &str) -> Option<f64> {
    run.thresholds
        .get(&format!("{key}_min"))
        .and_then(Value::as_f64)
}

/// A pair's render against the CPU reference's picture of it by the CPU
/// rule, as [`Run::cpu_check`] holds a frame's: a PSNR floor calibrated
/// when blessing (3 dB under what it measured, never below 36 dB) and no
/// channel more than 0.1 off. Gives the PSNR.
fn cpu_check(run: &mut Run<'_>, key: &str, got: &Image, cpu: &ImageF64) -> f64 {
    cpu_rule(run, key, (got.width, got.height), &unit(got), &cpu.data)
}

/// [`cpu_check`] on a render of `size` already in [0, 1].
fn cpu_rule(run: &mut Run<'_>, key: &str, size: (u32, u32), got: &[f64], cpu: &[f64]) -> f64 {
    let p = psnr_of(size, got, cpu);
    let ok = run.calibrated(&format!("{key}_psnr_db"), p, CPU_MARGIN_DB, CPU_FLOOR_DB);
    let m = max_diff(got, cpu);
    let ok = run.at_most(&format!("{key}_max_channel"), m, CPU_MAX_CHANNEL) && ok;
    if !ok {
        run.write_diffs(key, size, got, cpu);
    }
    p
}

/// The CPU reference's stitch of the far `SeamLines` pair through whole
/// view `k` by the true rig, made once for the binary.
fn cpu_view(k: usize) -> &'static ImageF64 {
    static VIEWS: [OnceLock<ImageF64>; 4] = [const { OnceLock::new() }; 4];
    VIEWS[k].get_or_init(|| {
        stitch(
            both(lines()),
            &x5().rig,
            UNIT,
            &whole_view(k),
            RigDebug::Off,
        )
    })
}

/// The pixels of whole view `k` that the stripe holding its band holds
/// ([`view`]), in the stripe's order.
fn stripe_of(whole: &[f64], k: usize) -> Vec<f64> {
    let (side, stripe) = (VIEW_SIDE as usize, STRIPE as usize);
    let inside = (side - stripe) / 2..(side + stripe) / 2;
    (0..side * side)
        .filter(|n| {
            let (x, y) = (n % side, n / side);
            inside.contains(if seam_runs_down(k) { &x } else { &y })
        })
        .flat_map(|n| whole[3 * n..3 * n + 3].iter().copied())
        .collect()
}

/// The manifest's view for a row that reads view `k`'s band: the whole
/// view, and the stripe of it that holds the band.
fn band_view_about(k: usize) -> Value {
    let (whole, size) = whole_view(k);
    let mut about = view_about(&whole, size);
    let (width, height) = view(k).1;
    about["band_stripe"] = json!({
        "size": [width, height],
        "of_the_view": if seam_runs_down(k) { "middle columns" } else { "middle rows" },
    });
    about
}

/// The manifest's view for a row that reads the seam's strips.
fn strips_about() -> Value {
    let (width, height) = SPEC.size();
    json!({
        "size": [width, height],
        "output": "seam strips, StripSpec::TIER1",
        "px_per_deg": SPEC.px_per_deg,
        "half_band_deg": SPEC.half_band_deg,
        "bin_deg": SPEC.bin_deg,
        "search_deg": SPEC.search_deg,
    })
}

// The seam as the GPU's strips measure it.

/// The seams of `pairs`, each `(truth, frames, rig)`: `frames` (by
/// track), `truth`'s pictures, unwrapped by `rig`, as 8.3 reads them. The
/// GPU draws each pair's strips in turn, and each is then read on a
/// thread of its own by [`seam_of`]. `None`, with the failure recorded,
/// if the GPU draws no strips for one.
fn gpu_seams(
    run: &mut Run<'_>,
    pairs: &[(&RigTruth, [&dyn VideoPlanes; 2], &Rig)],
) -> Option<Vec<Seam>> {
    let mut drawn = Vec::with_capacity(pairs.len());
    for (_, frames, rig) in pairs {
        let lenses = frames.map(|frame| LensFrame {
            frame,
            from: PlaneSource::Bytes,
        });
        match run.renderer.strips(run.gpu, lenses, rig, &SPEC) {
            Ok(strips) => {
                run.drew_strips(&strips);
                drawn.push(strips);
            }
            Err(e) => {
                run.fail(format!("the strip pass: {e} ({})", e.code()));
                return None;
            }
        }
    }
    std::thread::scope(|s| {
        let read: Vec<_> = pairs
            .iter()
            .zip(&drawn)
            .map(|((truth, _, rig), strips)| s.spawn(move || seam_of(truth, rig, strips)))
            .collect();
        Some(
            read.into_iter()
                .map(|r| r.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect(),
        )
    })
}

/// The seam of the GPU's `strips` of `truth`'s pictures unwrapped by
/// `rig`, in `attribution::Seam`'s fields by `Seam::of`'s rules: what
/// `rig` predicts for `truth` with no images (step 1), and what the
/// strips measure (step 2). `Seam::of` measures the CPU reference's strips
/// (`strips_of`), and takes no others, so this repeats its rules, and the
/// registration row holds the two to the same seam, bit for bit, on
/// `strips_of`'s strips ([`same_seam`]).
fn seam_of(truth: &RigTruth, rig: &Rig, strips: &Strips) -> Seam {
    let predicted = predict_bins(truth, 0, rig, &SPEC);
    let measured = bin_shifts(strips, &SPEC);
    let (sx, sy) = scales();
    let rms = |pairs: &mut dyn Iterator<Item = (f64, f64)>| {
        let (mut sum, mut n) = (0.0, 0.0);
        for (across, along) in pairs {
            sum += (across * sy).powi(2) + (along * sx).powi(2);
            n += 1.0;
        }
        (sum / n).sqrt()
    };
    let off_px = rms(&mut measured
        .iter()
        .zip(&predicted)
        .filter(|(_, p)| p.valid)
        .map(|(m, p)| (m.across_deg - p.across_deg, m.along_deg - p.along_deg)));
    let misregistration_px = rms(&mut measured
        .iter()
        .filter(|m| m.valid)
        .map(|m| (m.across_deg, m.along_deg)));
    let over_predicted: Vec<BinShift> = measured
        .iter()
        .zip(&predicted)
        .map(|(m, p)| BinShift {
            valid: m.valid && p.valid,
            ..*m
        })
        .collect();
    Seam {
        held: predicted.iter().filter(|p| p.valid).count(),
        want: signature(&predicted),
        got: signature(&over_predicted),
        off_px,
        own: signature(&measured),
        misregistration_px,
        predicted,
        measured,
    }
}

/// Whether two seams hold the same numbers: as `Debug` writes them, each
/// float in the fewest digits that read back to its bits, and NaN equal
/// to NaN.
fn same_seam(a: &Seam, b: &Seam) -> bool {
    let numbers = |s: &Seam| {
        let fields = (&s.predicted, &s.measured, s.want, s.got, s.own);
        format!("{:?}", (s.held, s.off_px, s.misregistration_px, fields))
    };
    numbers(a) == numbers(b)
}

/// Records each term the strips measured over the bins the prediction
/// holds, in degrees, under `key`.
fn measure_terms(run: &mut Run<'_>, key: &str, seam: &Seam) {
    for (name, t) in NAMES.iter().zip(terms(&seam.got)) {
        run.measure(&format!("{key}{name}_deg"), t);
    }
}

/// Step 3 on a seam: its measured terms land where `lands` says, read
/// against its prediction's. A field that lands elsewhere fails the row.
fn attributed(run: &mut Run<'_>, what: &str, seam: &Seam, lands: Lands) -> bool {
    match attribute_to(&seam.want, &seam.got, lands) {
        Ok(found) => {
            eprintln!("{}: {what}: {found}", run.row);
            true
        }
        Err((misfit, said)) => {
            run.fail(format!(
                "{what}: step 3 finds {misfit:?}: {said}\n    predicted {}\n    measured  {}",
                line(&seam.want),
                line(&seam.got)
            ));
            false
        }
    }
}

/// 4.2's checks on a true model's seam, `hold_seam`'s, held as the row's
/// so that a run gathers them: every bin measured, the strips within 0.1
/// px rms of each other, no term at 0.03° (step 3 finds the field
/// nowhere), and no NCC collapse.
fn holds(run: &mut Run<'_>, what: &str, seam: &Seam) {
    let bins = seam.measured.len() as f64;
    run.at_least("bins_measured", seam.own.valid_bins as f64, bins);
    run.at_most(
        "misregistration_px",
        seam.misregistration_px,
        MISREGISTRATION_PX,
    );
    under(run, "largest_term_deg", largest_term(&seam.own), TERM_DEG);
    let nowhere = attributed(run, what, seam, Lands::Nowhere);
    held(run, "lands_nowhere", nowhere);
    run.at_least("median_ncc", seam.own.median_ncc, NCC);
    run.measure("off_prediction_px", seam.off_px);
    measure_terms(run, "", seam);
}

/// Whether a control holds every bin the prediction holds to be
/// measured, or records how many were not.
#[derive(Clone, Copy, PartialEq)]
enum Unmeasured {
    Held,
    Recorded,
}

/// A planted error's seam, held as a control of the registration: every
/// bin predicted (and measured, unless `unmeasured` says to record it),
/// the strips misregistered past 0.1 px, a term past 0.03°, and the field
/// attributed by step 3 to the terms `named`.
fn control_seam(
    run: &mut Run<'_>,
    key: &str,
    what: &str,
    seam: &Seam,
    named: &'static [usize],
    unmeasured: Unmeasured,
) {
    let key = format!("control_{key}_");
    run.at_least(
        &format!("{key}bins_predicted"),
        seam.held as f64,
        seam.predicted.len() as f64,
    );
    let missed = seam.unmeasured() as f64;
    match unmeasured {
        Unmeasured::Held => {
            run.at_most(&format!("{key}bins_unmeasured"), missed, 0.0);
            run.measure(&format!("{key}off_prediction_px"), seam.off_px);
        }
        Unmeasured::Recorded => run.measure(&format!("{key}bins_unmeasured"), missed),
    }
    run.over(
        &format!("{key}misregistration_px"),
        seam.misregistration_px,
        MISREGISTRATION_PX,
    );
    let largest = largest_term(&seam.own);
    run.over(&format!("{key}largest_term_deg"), largest, TERM_DEG);
    measure_terms(run, &key, seam);
    let found = attributed(run, what, seam, Lands::On(named));
    held(run, &format!("{key}attributed"), found);
}

// The rows.

fn dfish_rect_seam_left(run: &mut Run<'_>) {
    rect_seam(run, 1);
}

fn dfish_rect_seam_right(run: &mut Run<'_>) {
    rect_seam(run, 0);
}

/// 4.2's "at most 1 dB below the better of the two lenses alone".
const BAND_DB: f64 = -1.0;

/// How far a stitch lies above the better of its two lenses alone, in
/// dB, from the band's three PSNRs.
fn over_the_better([stitched, first, second]: [f64; 3]) -> f64 {
    stitched - first.max(second)
}

/// A side seam of the far `SeamLines` pair, whole view `k` at 256²
/// (`synth::rung`'s −X on the left, +X on the right), stored, against the
/// CPU reference by the CPU rule and against the ideal in the band 4.2's
/// way: the stitch's PSNR over the true rig's band in the view's stripe,
/// to a floor 1 dB under what it measured when blessed, and at most 1 dB
/// under the better of its lenses alone over the same pixels.
///
/// The band's rule passes an error both lenses share, so its own control
/// is rung 1's, lens 1 yawed 0.5°, which moves the seam across itself on
/// a side seam. The row's control is 10.5's, the turn left out, held by
/// what it is: a field 90° in size by the prediction (`apart_deg`), far
/// past the strips' search; strips misregistered past 0.1 px, on the CPU
/// reference's strips as the lifted attribution measures the plant once
/// for the binary (`Plant::seam`); and the ideal's floor in the band
/// missed. Not by the NCC: on `SeamLines` the checker repeats inside the
/// search, and the bins lock a period away at a high NCC
/// (`Signature::median_ncc`).
fn rect_seam(run: &mut Run<'_>, k: usize) {
    let x5 = x5();
    let pair = lines_source();
    let whole = whole_view(k);
    let Some(got) = render(run, &pair, &x5.rig, &whole, DebugMode::Off) else {
        return;
    };
    cpu_check(run, "cpu", &got, cpu_view(k));

    // The band.
    let band: Vec<bool> = Scene::Lines.band(k).iter().map(|w| *w > 0.0).collect();
    let ideal = ideal_view(Pattern::SeamLines, k);
    let shot = view(k);
    let in_band = |run: &mut Run<'_>, rig: &Rig, debug: DebugMode| {
        render(run, &pair, rig, &shot, debug).map(|img| psnr_in(&band, &unit(&img), ideal))
    };
    let (Some(first), Some(second)) = (
        in_band(run, &x5.rig, DebugMode::Lens0),
        in_band(run, &x5.rig, DebugMode::Lens1),
    ) else {
        return;
    };
    let stitched = psnr_in(&band, &stripe_of(&unit(&got), k), ideal);
    run.measure("band_pixels", band.iter().filter(|b| **b).count() as f64);
    run.calibrated("ideal_band_psnr_db", stitched, IDEAL_MARGIN_DB, 0.0);
    run.measure("lens_0_band_psnr_db", first);
    run.measure("lens_1_band_psnr_db", second);
    let over = over_the_better([stitched, first, second]);
    run.at_least("band_over_better_db", over, BAND_DB);

    // The rule's control: lens 1 yawed 0.5°, lens 0 the true rig's.
    let yawed = Scene::Lines.turned(0);
    let (Some(yawed_stitch), Some(yawed_second)) = (
        in_band(run, &yawed, DebugMode::Off),
        in_band(run, &yawed, DebugMode::Lens1),
    ) else {
        return;
    };
    let over = over_the_better([yawed_stitch, first, yawed_second]);
    run.measure("control_yawed_band_over_better_db", over);
    run.over("control_yawed_under_band_rule_db", BAND_DB - over, 0.0);

    // The row's control: the turn left out.
    let no_turn = Plant::NoTurn.setup();
    let [apart, least, most] = apart_deg(&no_turn.truth, &no_turn.rig);
    run.measure("control_no_turn_apart_least_deg", least);
    run.measure("control_no_turn_apart_most_deg", most);
    run.over("control_no_turn_apart_deg", apart, SPEC.search_deg);
    let seam = Plant::NoTurn.seam();
    run.measure("control_no_turn_bins_predicted", seam.held as f64);
    run.measure("control_no_turn_median_ncc", seam.own.median_ncc);
    run.over(
        "control_no_turn_misregistration_px",
        seam.misregistration_px,
        MISREGISTRATION_PX,
    );
    let Some(no_turn_stitch) = in_band(run, &no_turn.rig, DebugMode::Off) else {
        return;
    };
    run.measure("control_no_turn_ideal_band_psnr_db", no_turn_stitch);
    if let Some(floor) = floor_of(run, "ideal_band_psnr_db") {
        run.over(
            "control_no_turn_under_ideal_floor_db",
            floor - no_turn_stitch,
            0.0,
        );
    }

    let side = ["right (+X)", "left (−X)"][k];
    let what = format!(
        "the far SeamLines pair across its {side} side seam, against the CPU reference and the \
         ideal in the band; controls: lens 1 yawed 0.5° (the band's rule), the turn left out \
         (the prediction's size, the misregistration, the ideal's floor)"
    );
    run.stored_pair(&what, &got, &pair, band_view_about(k));
}

/// The V6 camera's side: 384², a quarter of rung 1's render.
const V6_SIDE: u32 = 384;

/// The X5-like camera as a V6 calibration (`x5_like_v6`) at [`V6_SIDE`]²,
/// seeing `SeamLines` infinitely far through the reading its pictures are
/// rendered under (`attribution::rendered_reading`), and sphere's rigs of
/// it read as rendered and under {D5D6, D7 ByAxis} (8.3's `V6D7`).
struct V6 {
    truth: RigTruth,
    frames: [rung::LensFrame; 2],
    as_rendered: Rig,
    misread: Rig,
}

/// The V6 camera's conventions: the X5-like camera's, its terms read as
/// `v6`.
fn v6_planted(v6: spherecut_synth::lens::TruthV6) -> Planted {
    Planted { v6, ..x5().planted }
}

fn v6() -> &'static V6 {
    static V6: OnceLock<V6> = OnceLock::new();
    V6.get_or_init(|| {
        let cal = x5_like_v6();
        let sides = [V6_SIDE; 2];
        let read = |v6| sphere_rig(&cal, &v6_planted(v6), &far(), FRONT_TRACK, sides).unwrap();
        let truth = compose(&cal, &v6_planted(rendered_reading()), FRONT_TRACK, sides);
        let misread = reading(PairOrder::D5D6, false, PrismAt::D7, PrismOrder::ByAxis);
        V6 {
            frames: pair_through(Pattern::SeamLines, &truth),
            as_rendered: read(rendered_reading()),
            misread: read(misread),
            truth,
        }
    })
}

fn v6_source() -> PairSource<'static> {
    let v6 = v6();
    let geometry = pair_geometry(&v6_planted(rendered_reading()), &far(), f64::INFINITY);
    let frames: [&dyn VideoPlanes; 2] = [&v6.frames[0], &v6.frames[1]];
    PairSource::new(
        "v6_seamlines",
        frames,
        &x5_like_v6(),
        geometry,
        &v6.as_rendered,
        UNIT,
    )
}

/// The registration the GPU's strips measure (8.2, 8.3): the far
/// `SeamLines` pair through the true rig, held by 4.2's seam checks with
/// the misregistration within 0.1 px rms, and `strips_of`'s numbers
/// recorded beside. Its controls are 10.5's planted errors, each
/// misregistered past 0.1 px and attributed by step 3 to the terms its
/// geometry lands on: lens 1 rolled 0.5° (c0), tilted 0.5° by its pitch
/// (b1), f × 1.012 by map A (a0), and lens 1's centre 2 px along x and
/// the V6 camera under the wrong reading (both on the first harmonics,
/// a1, b1, c1 and d1). The V6 camera read as rendered is held to land
/// nowhere and to be misregistered at most 0.1 px rms, the reading's own
/// checks. At 384² the strips leave some of the V6 camera's bins
/// unmeasured (5 of 72 read as rendered, and over a quarter under the
/// wrong reading, where the field reaches 1.8°), which its seams record
/// and step 3 reads past, as it reads a field over the bins the estimator
/// holds; at 768² the instrument's matrix holds every bin of both
/// readings measured. The seven pairs' strips are drawn in turn and read
/// side by side ([`gpu_seams`]). The GPU's seams are read by [`seam_of`],
/// which repeats `Seam::of`'s rules, so the two are held to the same
/// seam on `strips_of`'s strips of the true rig, and, the check's control,
/// to another on the GPU's.
fn dfish_seam_registration(run: &mut Run<'_>) {
    let x5 = x5();
    let pair = lines_source();
    let first = &[A1, B1, C1, D1][..];
    let planted = [
        ("roll", "lens 1 rolled 0.5°", Plant::Roll, &[C0][..]),
        ("tilt", "lens 1 pitched 0.5°", Plant::Pitch, &[B1]),
        ("focal", "f × 1.012, C read as A", Plant::MapA, &[A0]),
        ("centre", "cx + 2 px on lens 1", Plant::Centre, first),
    ];
    let setups = planted.map(|(_, _, plant, _)| plant.setup());
    let v6 = v6();
    let v6_frames: [&dyn VideoPlanes; 2] = [&v6.frames[0], &v6.frames[1]];
    let mut pairs = vec![(&x5.truth, planes(both(lines())), &x5.rig)];
    pairs.extend(
        setups
            .iter()
            .map(|setup| (&setup.truth, planes(setup.frames), &setup.rig)),
    );
    pairs.push((&v6.truth, v6_frames, &v6.as_rendered));
    pairs.push((&v6.truth, v6_frames, &v6.misread));
    let Some(seams) = gpu_seams(run, &pairs) else {
        return;
    };
    let [seam, roll, tilt, focal, centre, as_rendered, misread] = &seams[..] else {
        unreachable!("seven pairs give seven seams");
    };

    holds(run, "the true rig", seam);
    let cpu = Plant::Truth.seam();
    run.measure("strips_of_misregistration_px", cpu.misregistration_px);
    run.measure("strips_of_median_ncc", cpu.own.median_ncc);
    for (name, t) in NAMES.iter().zip(terms(&cpu.got)) {
        run.measure(&format!("strips_of_{name}_deg"), t);
    }
    let apart = terms(&seam.got)
        .iter()
        .zip(terms(&cpu.got))
        .map(|(g, c)| (g - c).abs())
        .fold(0.0, worse);
    run.measure("gpu_against_strips_of_largest_term_deg", apart);
    match strips_of(&ready(both(lines())), &x5.rig, &SPEC) {
        Ok(strips) => {
            let copy = seam_of(&x5.truth, &x5.rig, &strips);
            held(run, "seam_of_is_seam_of", same_seam(&copy, &cpu));
        }
        Err(e) => run.fail(format!("strips_of: {e} ({})", e.code())),
    }
    let differ = !same_seam(seam, &cpu);
    held(run, "control_gpu_strips_not_seam_of", differ);

    for ((key, what, _, named), seam) in planted.into_iter().zip([roll, tilt, focal, centre]) {
        control_seam(run, key, what, seam, named, Unmeasured::Held);
    }

    run.at_most(
        "v6_as_rendered_misregistration_px",
        as_rendered.misregistration_px,
        MISREGISTRATION_PX,
    );
    run.measure(
        "v6_as_rendered_bins_unmeasured",
        as_rendered.unmeasured() as f64,
    );
    let nowhere = attributed(
        run,
        "the V6 camera read as rendered",
        as_rendered,
        Lands::Nowhere,
    );
    held(run, "v6_as_rendered_lands_nowhere", nowhere);
    let what = "the V6 terms read as {D5D6, D7 ByAxis}";
    control_seam(
        run,
        "v6_reading",
        what,
        misread,
        first,
        Unmeasured::Recorded,
    );

    let v6_pair = v6_source();
    run.end_pair_check(
        "the far SeamLines pair's seam on the GPU's strips through the true rig, misregistered \
         at most 0.1 px rms, strips_of's numbers beside; controls, each misregistered and \
         attributed by 8.3's step 3: lens 1 rolled 0.5°, lens 1 pitched 0.5°, f × 1.012 (map \
         A), cx + 2 px on lens 1, and the V6 camera at 384² read as {D5D6, D7 ByAxis}",
        &[&pair, &v6_pair],
        vec![strips_about()],
    );
}

/// 10.5's a0 for the scene 1.5 m away stitched at q = 0, and its
/// tolerance, in degrees: b q_scene for the X5's 31.5 mm.
const A0_AT_1M5_DEG: f64 = 1.20;
const A0_TOLERANCE_DEG: f64 = 0.05;

/// 4.2's stitch distance (3.3, 8.2): `SeamLines` 1.5 m away stitched at
/// q = 1/1.5, held by the seam's checks on the GPU's strips, the
/// misregistration within 0.1 px rms. Its control stitches it at q = 0:
/// a0 = 1.20° ± 0.05°, with d1 named too, which parallax along the seam
/// brings at −0.134 of a0 on this rig, held by step 3's ratio rule to the
/// ratio predicted.
fn dfish_stitch_distance(run: &mut Run<'_>) {
    let x5 = x5();
    let frames = planes(both(near_lines()));
    let pair = x5_source(Scene::NearLines.row(), frames, &near(), NEAR_M, UNIT);
    let pairs = [
        (&x5.near, frames, &x5.near_rig),
        (&x5.near, frames, &x5.rig),
    ];
    let Some(seams) = gpu_seams(run, &pairs) else {
        return;
    };
    let [seam, at_infinity] = &seams[..] else {
        unreachable!("two pairs give two seams");
    };
    holds(run, "stitched at q = 1/1.5", seam);
    let key = "control_q0_";
    run.over(
        &format!("{key}misregistration_px"),
        at_infinity.misregistration_px,
        MISREGISTRATION_PX,
    );
    let largest = largest_term(&at_infinity.own);
    run.over(&format!("{key}largest_term_deg"), largest, TERM_DEG);
    measure_terms(run, key, at_infinity);
    let (a0, d1) = (at_infinity.got.a0, at_infinity.got.d1);
    run.at_most(
        &format!("{key}a0_off_1_20_deg"),
        (a0.abs() - A0_AT_1M5_DEG).abs(),
        A0_TOLERANCE_DEG,
    );
    run.measure(&format!("{key}d1_of_a0"), d1 / a0);
    let want = &at_infinity.want;
    run.measure(&format!("{key}predicted_d1_of_a0"), want.d1 / want.a0);
    let what = "q = 0 against R = 1.5 m";
    let found = attributed(run, what, at_infinity, Lands::On(&[A0, D1]));
    held(run, &format!("{key}attributed"), found);
    run.end_pair_check(
        "SeamLines 1.5 m away on the GPU's strips stitched at q = 1/1.5, misregistered at most \
         0.1 px rms; control: stitched at q = 0, a0 = 1.20° ± 0.05° with d1 at its predicted \
         ratio, attributed to a0 and d1",
        &[&pair],
        vec![strips_about()],
    );
}

/// 4.2's bound on the step across the seam once matched, and 10.5's on
/// the feather's step between bins, in 8-bit steps.
const STEP_LSB: f64 = 1.0;

/// The step a control must reach, in 8-bit steps: 10.5's "at least 8".
const CONTROL_STEP_LSB: f64 = 8.0;

/// Lens 1's picture of the far `SeamLines`, rung 1's
/// (`RenderCache::shared`), under `gain` a channel: put on each pixel's
/// display value in linear light, as synth's exposure puts it on each
/// sample, clipped at white as the sensor clips, and coded as the X5
/// codes it. The pair's own picture is rendered once for the binary; a
/// gain on it costs no render of its own.
fn lens_1_at(gain: [f64; 3]) -> rung::LensFrame {
    let truth = &x5().truth;
    let picture = RenderCache::shared().render(Pattern::SeamLines, truth, 1, 0, SS);
    let pixels = picture
        .pixels
        .iter()
        .map(|c| std::array::from_fn(|k| oetf((eotf(c[k]) * gain[k]).min(1.0))))
        .collect();
    let exposed = RgbImage {
        width: picture.width,
        height: picture.height,
        pixels,
    };
    rung::LensFrame::new(lens_planes(&exposed, Coding::X5), truth.lenses[1].side)
}

/// Lens 1 at +0.5 EV and +5 % red, the lifted attribution's planted gain
/// (`attribution::gained`), the exposure match's.
fn gained_lens_1() -> &'static rung::LensFrame {
    static FRAME: OnceLock<rung::LensFrame> = OnceLock::new();
    FRAME.get_or_init(|| lens_1_at(gained().lenses[1].gain))
}

/// The exposure match (4.2, 10.3): lens 1 at +0.5 EV and +5 % red
/// ([`gained_lens_1`]), matched by the frame's gains for block 1 (the
/// planted gains' inverse), each lens alone in the shader's modes 4 and
/// 5, which end with the gain as the stitch does, on the equirect: the
/// mean step across the seam over the pixels both lenses cover and
/// neither clips, under 1 LSB in every channel. Control: the gains off, a
/// step of at least 8 LSB.
fn dfish_exposure_match(run: &mut Run<'_>) {
    let x5 = x5();
    let planted = gained().lenses[1].gain;
    let gains = [[1.0; 3], planted.map(|g| 1.0 / g)];
    let frames: [&dyn VideoPlanes; 2] = [&lines()[0], gained_lens_1()];
    let name = "x5_seamlines, lens 1 at +0.5 EV and +5 % red";
    let matched = x5_source(name, frames, &far(), f64::INFINITY, gains);
    let off = x5_source(name, frames, &far(), f64::INFINITY, UNIT);
    let shot = equirect();
    let (Some(lens_0), Some(lens_1), Some(lens_1_off)) = (
        render(run, &matched, &x5.rig, &shot, DebugMode::Lens0),
        render(run, &matched, &x5.rig, &shot, DebugMode::Lens1),
        render(run, &off, &x5.rig, &shot, DebugMode::Lens1),
    ) else {
        return;
    };
    let [lens_0, lens_1, lens_1_off] = [&*lens_0, &*lens_1, &*lens_1_off].map(unit);
    // Where both lenses cover, less the pixels where either at unit gain
    // passes 95 % of the display's light, which a gain could carry past
    // white.
    let overlap = &Scene::Lines.masks().overlap;
    let clip = srgb_oetf(0.95);
    let clips = |image: &[f64], n: usize| image[3 * n..3 * n + 3].iter().any(|v| *v > clip);
    let kept: Vec<usize> = (0..overlap.len())
        .filter(|&n| overlap[n] > 0.0 && !clips(&lens_0, n) && !clips(&lens_1_off, n))
        .collect();
    run.at_least("step_pixels", kept.len() as f64, 10_000.0);
    let step = |lens_1: &[f64]| -> [f64; 3] {
        let mut sum = [0.0; 3];
        for &n in &kept {
            for (c, sum) in sum.iter_mut().enumerate() {
                *sum += lens_1[3 * n + c] - lens_0[3 * n + c];
            }
        }
        sum.map(|s| s / kept.len() as f64 * 255.0)
    };
    let worst = |by_channel: [f64; 3]| by_channel.iter().map(|s| s.abs()).fold(0.0, worse);
    let (matched_step, off_step) = (step(&lens_1), step(&lens_1_off));
    for (c, channel) in ["r", "g", "b"].iter().enumerate() {
        run.measure(&format!("step_{channel}_lsb"), matched_step[c]);
        run.measure(
            &format!("control_gains_off_step_{channel}_lsb"),
            off_step[c],
        );
    }
    under(run, "step_lsb", worst(matched_step), STEP_LSB);
    run.at_least(
        "control_gains_off_step_lsb",
        worst(off_step),
        CONTROL_STEP_LSB,
    );
    run.end_pair_check(
        "lens 1 at +0.5 EV and +5 % red matched by the frame's gains, each lens alone (modes 4 \
         and 5) on the equirect: the mean step across the seam where both cover, under 1 LSB a \
         channel; control: the gains off, at least 8 LSB",
        &[&matched, &off],
        vec![view_about(&shot.0, shot.1)],
    );
}

/// The feather's bins of angle from the seam, in degrees: 10.5's 0.45°,
/// a pixel's span at a 256² view's centre.
const FEATHER_BIN_DEG: f64 = 0.45;

/// How many bins the feather reads either side of the seam: 16, out to
/// 7.2°, inside the stripe's ±7.5° at its view's edge, so that every bin
/// holds pixels from the whole length of the stripe.
const FEATHER_BINS: usize = 16;

/// Lens 1 at +0.25 EV, 10.5's feather.
fn quarter_ev_lens_1() -> &'static rung::LensFrame {
    static FRAME: OnceLock<rung::LensFrame> = OnceLock::new();
    FRAME.get_or_init(|| lens_1_at([2f64.powf(0.25); 3]))
}

/// The angle of rig direction `d` from `rig`'s seam, in degrees: half of
/// how much nearer it lies to lens 0's band centre than to lens 1's, by
/// θ_b − c_b on each lens's parallax-corrected ray, so 0 where a hard
/// switch changes lens and positive on lens 0's side.
fn from_seam_deg(rig: &Rig, d: DVec3) -> f64 {
    let past = |b: usize| {
        let ray = rig.ray(b, d);
        ray.x.hypot(ray.y).atan2(ray.z) - rig.band_centre(b)
    };
    ((past(1) - past(0)) / 2.0).to_degrees()
}

/// Each pixel of view `k`'s stripe ([`view`]) by its bin of angle from
/// the true rig's seam, counted from lens 0's side: `None` past
/// [`FEATHER_BINS`] of the seam. Worked out once for the binary.
fn feather_bins(k: usize) -> &'static [Option<usize>] {
    static BINS: [OnceLock<Vec<Option<usize>>>; 4] = [const { OnceLock::new() }; 4];
    BINS[k].get_or_init(|| {
        let rig = &x5().rig;
        let (v, size) = view(k);
        let reach = FEATHER_BINS as f64;
        (0..size.1)
            .flat_map(|y| (0..size.0).map(move |x| (x, y)))
            .map(|(x, y)| {
                let d = v.dir(size, f64::from(x) + 0.5, f64::from(y) + 0.5);
                let from_lens_0 = reach - (from_seam_deg(rig, d) / FEATHER_BIN_DEG).floor() - 1.0;
                (0.0..2.0 * reach)
                    .contains(&from_lens_0)
                    .then_some(from_lens_0 as usize)
            })
            .collect()
    })
}

/// The mean of `a − b`, two renders of view `k`'s stripe, in each bin of
/// [`feather_bins`], with the pixels it is over: in 8-bit steps, over the
/// three channels of each pixel.
fn by_angle(a: &[f64], b: &[f64], k: usize) -> Vec<(f64, usize)> {
    let mut sums = vec![(0.0, 0); 2 * FEATHER_BINS];
    for (n, bin) in feather_bins(k).iter().enumerate() {
        let Some(bin) = bin else {
            continue;
        };
        let d = (3 * n..3 * n + 3).map(|c| a[c] - b[c]).sum::<f64>() / 3.0;
        let (sum, count) = &mut sums[*bin];
        *sum += d * 255.0;
        *count += 1;
    }
    sums.into_iter()
        .map(|(sum, count)| (sum / count.max(1) as f64, count))
        .collect()
}

/// The largest change of the mean between bins `apart` bins from each
/// other, in 8-bit steps: 1 for neighbours. NaN if one is.
fn largest_step(bins: &[(f64, usize)], apart: usize) -> f64 {
    bins.iter()
        .zip(&bins[apart..])
        .map(|(a, b)| (b.0 - a.0).abs())
        .fold(0.0, worse)
}

/// The feather's control, a hard switch: the true rig's tune with h = 0,
/// made once for the binary.
fn hard() -> &'static Rig {
    static RIG: OnceLock<Rig> = OnceLock::new();
    RIG.get_or_init(|| tuned(|t| t.feather_deg = 0.0))
}

/// The CPU reference's stitch of the far `SeamLines` pair through view
/// `k`'s stripe by the hard switch ([`hard`]), at unit gains, made once
/// for the binary.
fn cpu_hard_stripe(k: usize) -> &'static ImageF64 {
    static STRIPES: [OnceLock<ImageF64>; 4] = [const { OnceLock::new() }; 4];
    STRIPES[k].get_or_init(|| stitch(both(lines()), hard(), UNIT, &view(k), RigDebug::Off))
}

/// How many pixels of view `k`'s stripe the hard switch's step gives to
/// neither lens alone, where both cover and each lies on the same side of
/// its band's centre, so that `Rig::weights`' rule decides them (F025):
/// those where both lie inside, which it gives ½ each, and those where
/// both lie past, which it gives the nearer lens.
fn decided_by_the_rule(k: usize) -> [usize; 2] {
    let rig = hard();
    let (v, size) = view(k);
    let mut counts = [0; 2];
    for (x, y) in (0..size.1).flat_map(|y| (0..size.0).map(move |x| (x, y))) {
        let d = v.dir(size, f64::from(x) + 0.5, f64::from(y) + 0.5);
        if !(0..2).all(|b| rig.covers(b, d, 0.0)) {
            continue;
        }
        let inside = [0, 1].map(|b| {
            let ray = rig.ray(b, d);
            ray.x.hypot(ray.y).atan2(ray.z) < rig.band_centre(b)
        });
        match inside {
            [true, true] => counts[0] += 1,
            [false, false] => counts[1] += 1,
            _ => {}
        }
    }
    counts
}

/// `image`, a render of view `k`'s stripe, with a dark ring planted on
/// the seam: the pixels of the two bins of [`feather_bins`] that the seam
/// parts black, as a stitch shows them that weighs neither lens there.
fn ringed(image: &[f64], k: usize) -> Vec<f64> {
    let seam = FEATHER_BINS - 1..=FEATHER_BINS;
    let mut out = image.to_vec();
    for (n, bin) in feather_bins(k).iter().enumerate() {
        if bin.is_some_and(|b| seam.contains(&b)) {
            out[3 * n..3 * n + 3].fill(0.0);
        }
    }
    out
}

/// The fewest pixels a feather bin may hold.
const FEATHER_MIN_PIXELS: f64 = 50.0;

/// The step between neighbouring bins a hard switch must reach, in 8-bit
/// steps: half of 10.5's 8 LSB, the share of a whole step a bin edge
/// leaves to each of the two bins its seam crosses.
const CONTROL_NEIGHBOUR_LSB: f64 = 4.0;

/// The feather (6.2, 10.3): lens 1 at +0.25 EV ([`quarter_ev_lens_1`])
/// with the gains off, 10.6 LSB brighter than lens 0 at mid-grey, on the
/// four seam views over `SeamLines`' ramp, the stripe of each that holds
/// the band, each against the same view of the pair at unit gains: the
/// mean of the difference, in f64, over the pixels in each 0.45° of angle
/// from the seam, changes by under 1 LSB between neighbouring bins. What
/// the two renders share cancels: the scene's own detail, which against
/// the ideal moves a bin's mean by up to 2 LSB on its own (recorded
/// beside, `against_ideal_step_lsb`), and the stitch's own error. What is
/// left is the gain's step, as the feather spreads it.
///
/// Its control is a hard switch, the feather's h = 0, rendered the same
/// two ways, whose whole step falls between two bins: at least 4 LSB
/// between neighbours on every view, half of it on either side of a bin
/// edge at worst, and 10.5's 8 LSB between bins two apart on the views
/// whose seam lies at mid-grey or brighter. +X's lies at the ramp's dark
/// end, and its step two bins apart is recorded and not held.
///
/// A hard switch leaves pixels at the seam to the weights rule (F025):
/// where both lenses lie inside their band's centre it gives ½ each, and
/// where both lie past it the nearer lens, and how many of each the
/// stripe holds is recorded. A fault there that both renders share, a
/// dark ring where the stitch weighs neither lens, would cancel in the
/// difference, and no other row stitches at h = 0 (the side seams' rows
/// stitch at the default tune, where both feathers weigh across the
/// seam), so the switch's stripe at unit gains is held to the CPU
/// reference's stitch of it by the CPU rule. Its control is that stripe
/// with a dark ring planted on the seam ([`ringed`]), which must miss the
/// rule: a channel more than 0.1 off, and the PSNR under the floor.
fn dfish_feather(run: &mut Run<'_>) {
    let x5 = x5();
    let frames: [&dyn VideoPlanes; 2] = [&lines()[0], quarter_ev_lens_1()];
    let name = "x5_seamlines, lens 1 at +0.25 EV";
    let pair = x5_source(name, frames, &far(), f64::INFINITY, UNIT);
    let plain = lines_source();
    let hard = hard();
    for (k, key) in VIEW_KEYS.iter().enumerate() {
        let shot = view(k);
        let renders = [
            (&pair, &x5.rig),
            (&plain, &x5.rig),
            (&pair, hard),
            (&plain, hard),
        ]
        .map(|(source, rig)| render(run, source, rig, &shot, DebugMode::Off).map(|i| unit(&i)));
        let [
            Some(got),
            Some(at_unit),
            Some(switched),
            Some(switched_at_unit),
        ] = renders
        else {
            return;
        };
        let feathered = by_angle(&got, &at_unit, k);
        let fewest = feathered.iter().map(|b| b.1).min().unwrap_or(0);
        run.at_least(
            &format!("{key}_pixels_a_bin"),
            fewest as f64,
            FEATHER_MIN_PIXELS,
        );
        let across = feathered.last().map_or(f64::NAN, |b| b.0) - feathered[0].0;
        run.measure(&format!("{key}_across_lsb"), across);
        under(
            run,
            &format!("{key}_step_lsb"),
            largest_step(&feathered, 1),
            STEP_LSB,
        );
        let ideal = ideal_view(Pattern::SeamLines, k);
        let against_ideal = largest_step(&by_angle(&got, ideal, k), 1);
        run.measure(&format!("{key}_against_ideal_step_lsb"), against_ideal);

        let switched = by_angle(&switched, &switched_at_unit, k);
        let control = format!("control_h0_{key}");
        run.at_least(
            &format!("{control}_step_lsb"),
            largest_step(&switched, 1),
            CONTROL_NEIGHBOUR_LSB,
        );
        let whole = largest_step(&switched, 2);
        if k == 0 {
            run.measure(&format!("{control}_two_bins_lsb"), whole);
        } else {
            run.at_least(&format!("{control}_two_bins_lsb"), whole, CONTROL_STEP_LSB);
        }

        // The switch's own light, which the difference cancels.
        let [halves, nearer] = decided_by_the_rule(k);
        run.measure(&format!("h0_{key}_halves_pixels"), halves as f64);
        run.measure(&format!("h0_{key}_nearer_pixels"), nearer as f64);
        let cpu = &cpu_hard_stripe(k).data;
        let cpu_key = format!("h0_{key}_cpu");
        cpu_rule(run, &cpu_key, shot.1, &switched_at_unit, cpu);
        let ring = ringed(&switched_at_unit, k);
        let off = max_diff(&ring, cpu);
        run.over(&format!("{control}_ring_max_channel"), off, CPU_MAX_CHANNEL);
        if let Some(floor) = floor_of(run, &format!("{cpu_key}_psnr_db")) {
            let p = psnr_of(shot.1, &ring, cpu);
            run.measure(&format!("{control}_ring_psnr_db"), p);
            run.over(
                &format!("{control}_ring_under_cpu_floor_db"),
                floor - p,
                0.0,
            );
        }
    }
    let views = (0..VIEWS.len()).map(band_view_about).collect();
    run.end_pair_check(
        "lens 1 at +0.25 EV with the gains off on the four seam views' stripes, against the pair \
         at unit gains: the mean difference by 0.45° of angle from the seam changes by under 1 \
         LSB a bin; control: h = 0, at least 4 LSB between neighbouring bins, and 8 LSB between \
         bins two apart on −X, +Y and −Y; h = 0 at unit gains against the CPU reference by the \
         CPU rule, control: a dark ring planted on the seam",
        &[&pair, &plain],
        views,
    );
}

/// A lens's planes under another colour: the X5's planes tagged limited
/// range, the full-range row's control.
#[derive(Debug)]
struct Tagged {
    frame: &'static rung::LensFrame,
    color: ColorInfo,
}

impl VideoPlanes for Tagged {
    fn layout(&self) -> &PixelLayout {
        self.frame.layout()
    }

    fn size(&self) -> (u32, u32) {
        self.frame.size()
    }

    fn plane(&self, index: usize) -> Option<PlaneData<'_>> {
        self.frame.plane(index)
    }

    fn color(&self) -> &ColorInfo {
        &self.color
    }
}

/// The X5's own coding (4.2, 7.1): 8-bit 4:2:0, full range, BT.709,
/// chroma sited left on both tracks, as synth codes rung 1's pairs,
/// stitched through the right side seam's view and held to the CPU
/// reference by the CPU rule. Control: the same planes tagged limited
/// range, which the coding's flag tells apart, at least 10 dB further
/// from the reference.
fn dfish_fullrange_left(run: &mut Run<'_>) {
    let x5 = x5();
    let pair = lines_source();
    let x5_coded = |frames: [&dyn VideoPlanes; 2]| {
        frames.iter().all(|frame| {
            let coded = frame.color();
            (coded.range, coded.matrix, coded.siting)
                == (Range::Full, Matrix::Bt709, ChromaSiting::Left)
        })
    };
    held(
        run,
        "coded_full_bt709_left",
        x5_coded(planes(both(lines()))),
    );
    let tagged = lines().each_ref().map(|frame| Tagged {
        frame,
        color: ColorInfo {
            range: Range::Limited,
            ..*frame.color()
        },
    });
    let not_x5 = !x5_coded([&tagged[0], &tagged[1]]);
    held(run, "control_limited_not_full_bt709_left", not_x5);
    let name = Scene::Lines.row();
    let limited = x5_source(name, [&tagged[0], &tagged[1]], &far(), f64::INFINITY, UNIT);
    let k = 0;
    let shot = whole_view(k);
    let (Some(got), Some(as_limited)) = (
        render(run, &pair, &x5.rig, &shot, DebugMode::Off),
        render(run, &limited, &x5.rig, &shot, DebugMode::Off),
    ) else {
        return;
    };
    let cpu = cpu_view(k);
    let p = cpu_check(run, "cpu", &got, cpu);
    let wrong = psnr_of(shot.1, &unit(&as_limited), &cpu.data);
    run.measure("control_limited_psnr_db", wrong);
    run.at_least("control_limited_db_off", p - wrong, 10.0);
    run.end_pair_check(
        "the X5's coding (8-bit 4:2:0, full range, BT.709, chroma left) through +X, against the \
         CPU reference; control: the same planes tagged limited, at least 10 dB off",
        &[&pair, &limited],
        vec![view_about(&shot.0, shot.1)],
    );
}

/// The layouts (6.2, 10.5): the far `SeamLines` pair coded as NV12, I420
/// and P010 in the X5's colour (full range, BT.709, chroma left),
/// stitched through the right side seam's view, each two within the CPU
/// rule of each other. NV12 and I420 carry the same 8-bit samples, laid
/// out two ways, and are held to the same pixels, as `tenbit_ramp` holds
/// P010 and I010: a PSNR between them is infinite, which no floor can be
/// blessed at. No control: 10.5 gives none.
fn dfish_layout_invariance(run: &mut Run<'_>) {
    let x5 = x5();
    let kinds = [Kind::Nv12, Kind::I420, Kind::P010];
    let coded = codings(
        &kinds,
        color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
    );
    let name = Scene::Lines.row();
    let sources: Vec<PairSource<'_>> = coded
        .iter()
        .map(|[a, b]| x5_source(name, [a, b], &far(), f64::INFINITY, UNIT))
        .collect();
    let shot = whole_view(0);
    let mut renders = Vec::new();
    for source in &sources {
        let Some(img) = render(run, source, &x5.rig, &shot, DebugMode::Off) else {
            return;
        };
        renders.push(unit(&img));
    }
    for (i, j) in [(0, 1), (0, 2), (1, 2)] {
        let key = format!("{}_against_{}", kinds[i].name(), kinds[j].name());
        let (a, b) = (&renders[i], &renders[j]);
        let ok = if kinds[i].bits() == kinds[j].bits() {
            held(run, &format!("{key}_identical"), a == b)
        } else {
            let p = psnr_of(shot.1, a, b);
            run.calibrated(&format!("{key}_psnr_db"), p, CPU_MARGIN_DB, CPU_FLOOR_DB)
        };
        let m = max_diff(a, b);
        if !(run.at_most(&format!("{key}_max_channel"), m, CPU_MAX_CHANNEL) && ok) {
            run.write_diffs(&key, shot.1, a, b);
        }
    }
    run.end_pair_check(
        "the far SeamLines pair as NV12, I420 and P010 (full range, BT.709, chroma left) through \
         +X, each two within the CPU rule of each other, NV12 and I420 the same pixels",
        &sources.iter().collect::<Vec<_>>(),
        vec![view_about(&shot.0, shot.1)],
    );
}

/// The far `SeamLines` pair's two pictures as synth renders them for
/// rung 1 (`RenderCache::shared`), coded as each of `kinds` in `color`,
/// by track.
fn codings(kinds: &[Kind], color: ColorInfo) -> Vec<[Frame; 2]> {
    let truth = &x5().truth;
    std::thread::scope(|s| {
        let coded: Vec<_> = kinds
            .iter()
            .map(|&kind| {
                s.spawn(move || {
                    [0, 1].map(|track| {
                        let shared = RenderCache::shared();
                        let picture = shared.render(Pattern::SeamLines, truth, track, 0, SS);
                        frame(&picture, kind, color, 0)
                    })
                })
            })
            .collect();
        coded
            .into_iter()
            .map(|c| c.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}
