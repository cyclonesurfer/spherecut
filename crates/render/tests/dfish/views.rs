//! The views' dual-fisheye goldens (M3 plan 10.5), a lens pair stitched
//! by the shader and seen whole: the equirect, the poles against their
//! twins, each lens alone, a minified tiny planet, the LOD map, the mask's
//! margin and frame term, and the Kannala–Brandt rig under each base.
//! Rows of the goldens binary, blessed into `goldens/dfish_views/` with a
//! manifest of their own (see the binary's doc).
//!
//! Most rows render rung 1's X5-like camera at 768² a lens
//! (`common::rung1`, `synth::rung`), the far `Grid` pair or, where a row
//! names no scene, the far `SeamLines` pair, from the pairs the binary
//! renders once, and record each pair as 10.5 asks through the run's pair
//! API. The tiny planet and the mask's rows render the same camera at
//! 384² seeing `HighFrequency`, whose 1° checker a minified pixel averages
//! to a flat grey; the mask's margin reads a second such pair whose truth
//! is black past the rig's θ_max, so that the black sits where the mask
//! must hold the mips' bleed off. The Kannala–Brandt row renders
//! `kb_like`'s camera on each of the four bases at 256², each its own pair.
//! The CPU's share of the rows (the pairs, the ideals, the true rig's
//! masks and the CPU reference's pictures) is made on threads beside the
//! GPU's draws ([`MADE_BESIDE`]).

use std::f64::consts::PI;
use std::sync::OnceLock;

use glam::DQuat;
use serde_json::{Value, json};
use spherecut_core::video::VideoPlanes;
use spherecut_render::compare::sample_diff;
use spherecut_render::plane::mip_levels;
use spherecut_render::readback::round_f16;
use spherecut_render::reference::{self, ImageF64, RigDebug, lens_footprint};
use spherecut_render::seam::lens_coverage;
use spherecut_render::{DebugMode, Depth, Image, StillOptions, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::Projection;
use spherecut_sphere::pixel;
use spherecut_sphere::rig::{Rig, RigLens, Tune};
use spherecut_synth::lens::{IMAGE_CIRCLE_DEG, KB_LIKE_K, KbBase, TruthModel, compose, kb_like};
use spherecut_synth::rung::{FRONT_TRACK, LensFrame, far, pair_through};
use spherecut_synth::{Pattern, render_view_ideal};

use super::common::attribution::{equidistant_for, equisolid_for};
use super::common::rig::{rig_of_truth, sphere_rig};
use super::common::rung1::{
    self, Pair, Scene, Shot, UNIT, both, equirect, grid, ideal_equirect, lines, ready, stitch,
    tuned, whole_view, x5,
};
use super::{
    CONTROL_BAD_OVER, CPU_FLOOR_DB, CPU_MARGIN_DB, CPU_MAX_CHANNEL, IDEAL_MARGIN_DB, IDEAL_MIN_LOD,
    IDEAL_N_CHECKER, LOWER, Mode, PairSource, Row, Run, SQUARE, Stitched, TWIN_THRESHOLD, UPDATE,
    bad, bless, diff_dir_beside, gpu, ideal_check, ideal_dir, ideal_dir_in, lens, max_diff, num,
    pair_geometry, psnr_of, rgb_unit, run_these, sides, unit, view_about, ypr,
};

/// Where these rows' images and manifest live.
pub(super) const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/dfish_views");

/// What their manifest says it is.
pub(super) const ABOUT: &str = "The views' dual-fisheye goldens (M3 plan 10.5, DECISIONS D32): \
                                lens pairs rendered from synth's planes and stitched by the \
                                shader on lavapipe, with each check's thresholds and what it \
                                measured when blessed.";

/// The rows, in 10.5's order.
pub(super) const ROWS: &[(&str, Row)] = &[
    ("dfish_equirect_identity", dfish_equirect_identity),
    ("dfish_zenith", dfish_zenith),
    ("dfish_nadir", dfish_nadir),
    ("dfish_lens_only_0", dfish_lens_only_0),
    ("dfish_lens_only_1", dfish_lens_only_1),
    ("dfish_hifreq_tinyplanet", dfish_hifreq_tinyplanet),
    ("dfish_lod_heatmap", dfish_lod_heatmap),
    ("dfish_mask_margin", dfish_mask_margin),
    ("dfish_kb_rig", dfish_kb_rig),
];

#[test]
fn dfish_view_rows() {
    let gpu = gpu();
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let diffs = diff_dir_beside("dfish-views");
    let _ = std::fs::remove_dir_all(&diffs);
    let mode = Mode::Golden {
        bless: bless::on(UPDATE),
    };
    let mut run = Run::at(gpu, mode, std::path::Path::new(DIR), &diffs);
    run.about = ABOUT;
    run.lower_floors = bless::lower(UPDATE);
    run.tell = true;
    std::thread::scope(|s| {
        for make in MADE_BESIDE {
            s.spawn(make);
        }
        let (projector, filter) = (run.renderer.projector(), StillOptions::default().filter);
        std::thread::scope(|p| {
            for debug in MODES {
                p.spawn(move || projector.prepare_rig(filter, debug));
            }
        });
        run_these(&mut run, ROWS, None);
    });
    run.finish();
}

// The pairs.

/// The X5-like camera's pair `frames` (by track) of the scene `name`,
/// with its rig built under `tune`, the scene infinitely far, at unit
/// gains.
fn x5_source<'a>(name: &str, frames: [&'a dyn VideoPlanes; 2], tune: &Tune) -> PairSource<'a> {
    let x5 = x5();
    let geometry = pair_geometry(&x5.planted, tune, f64::INFINITY);
    let rig = sphere_rig(&x5.cal, &x5.planted, tune, FRONT_TRACK, sides(frames)).unwrap();
    PairSource::new(name, frames, &x5.cal, geometry, &rig, UNIT)
}

/// One of rung 1's pairs as the planes a source records.
fn planes(pair: Pair) -> [&'static dyn VideoPlanes; 2] {
    [pair[0], pair[1]]
}

/// The side of the `HighFrequency` pair's lenses, and of its rig's slots.
const HIFREQ_SIDE: u32 = 384;

/// What a `HighFrequency` pair's truth shows: the X5-like truth's content,
/// out to 106° and past the slot's frame, or black past the rig's θ_max.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    ToTheCircle,
    ToTheMask,
}

/// The X5-like camera at [`HIFREQ_SIDE`]² a lens seeing `HighFrequency`
/// infinitely far, and sphere's rig of it built at that side.
struct HiFreq {
    frames: [LensFrame; 2],
    rig: Rig,
}

fn hifreq(content: Content) -> &'static HiFreq {
    static PAIRS: [OnceLock<HiFreq>; 2] = [const { OnceLock::new() }; 2];
    PAIRS[content as usize].get_or_init(|| {
        let x5 = x5();
        let sides = [HIFREQ_SIDE; 2];
        let rig = sphere_rig(&x5.cal, &x5.planted, &far(), FRONT_TRACK, sides).unwrap();
        let mut truth = compose(&x5.cal, &x5.planted, FRONT_TRACK, sides);
        if content == Content::ToTheMask {
            for block in &rig.lenses {
                truth.lenses[block.track].theta_max = block.theta_max;
            }
        }
        HiFreq {
            frames: pair_through(Pattern::HighFrequency, &truth),
            rig,
        }
    })
}

impl HiFreq {
    fn source(&self, name: &str, tune: &Tune) -> PairSource<'_> {
        x5_source(name, [&self.frames[0], &self.frames[1]], tune)
    }
}

/// The Kannala–Brandt bases `dfish_kb_rig` renders, each its own pair.
const KB_BASES: [KbBase; 4] = [
    KbBase::Equidistant,
    KbBase::Equisolid,
    KbBase::Stereographic,
    KbBase::Orthographic,
];

/// The side of those pairs' lenses.
const KB_SIDE: u32 = 256;

/// `kb_like`'s camera at [`KB_SIDE`]² a lens, on one of the four bases,
/// seeing `SeamLines` infinitely far, and sphere's rig of that truth.
struct KbPair {
    frames: [LensFrame; 2],
    rig: Rig,
    /// The lenses the rig is built from, for the manifest: the base, the
    /// polynomial and each track's focal and centre.
    lenses: Value,
}

/// `kb_like`'s camera on `KB_BASES[k]`: each lens's focal set, as rung 0
/// sets it (`synth/tests/lens_against_sphere.rs`), so that at 106° the
/// base reaches as far as the equidistant lens does; the orthographic
/// base images to θ_d = 90° alone, so its focal is the reach itself, and
/// each lens shows nothing past about 88°, short of the seam.
fn kb_on(k: usize) -> &'static KbPair {
    static PAIRS: [OnceLock<KbPair>; 4] = [const { OnceLock::new() }; 4];
    PAIRS[k].get_or_init(|| {
        let x5 = x5();
        let mut truth = kb_like(&compose(&x5.cal, &x5.planted, FRONT_TRACK, [KB_SIDE; 2]));
        let theta = IMAGE_CIRCLE_DEG.to_radians();
        let t2 = theta * theta;
        let c = KB_LIKE_K;
        let theta_d =
            theta * (1.0 + t2 * (c[0] + t2 * (c[1] + t2 * (c[2] + t2 * (c[3] + t2 * c[4])))));
        let radius = match KB_BASES[k] {
            KbBase::Equidistant => theta_d,
            KbBase::Equisolid => 2.0 * (theta_d / 2.0).sin(),
            KbBase::Stereographic => 2.0 * (theta_d / 2.0).tan(),
            KbBase::Orthographic => 1.0,
        };
        let mut tracks = Vec::with_capacity(truth.lenses.len());
        for lens in &mut truth.lenses {
            let TruthModel::Kb {
                base,
                fx,
                fy,
                cx,
                cy,
                ..
            } = &mut lens.model
            else {
                panic!("kb_like makes Kannala–Brandt lenses");
            };
            let reach = *fx * theta_d;
            *base = KB_BASES[k];
            *fx = reach / radius;
            *fy = reach / radius;
            tracks.push(json!({
                "focal_px": [num(*fx), num(*fy)],
                "centre_px": [num(*cx), num(*cy)],
            }));
        }
        KbPair {
            frames: pair_through(Pattern::SeamLines, &truth),
            rig: rig_of_truth(&truth, &far()),
            lenses: json!({
                "model": "Kannala–Brandt",
                "base": format!("{:?}", KB_BASES[k]).to_lowercase(),
                "k": c,
                "by_track": tracks,
            }),
        }
    })
}

/// The CPU reference's stitch of the far `Grid` pair through the
/// equirect, made once for the binary.
fn cpu_grid_equirect() -> &'static ImageF64 {
    static PICTURE: OnceLock<ImageF64> = OnceLock::new();
    PICTURE.get_or_init(|| stitch(both(grid()), &x5().rig, UNIT, &equirect(), RigDebug::Off))
}

/// The CPU reference's stitch of `kb_on(k)` through the equirect, made
/// once for the binary.
fn cpu_kb_equirect(k: usize) -> &'static ImageF64 {
    static PICTURES: [OnceLock<ImageF64>; 4] = [const { OnceLock::new() }; 4];
    PICTURES[k].get_or_init(|| {
        let kb = kb_on(k);
        stitch(both(&kb.frames), &kb.rig, UNIT, &equirect(), RigDebug::Off)
    })
}

/// The CPU reference's stitch of the `HighFrequency` pair at 384² through
/// the tiny planet, made once for the binary.
fn cpu_hifreq_planet() -> &'static ImageF64 {
    static PICTURE: OnceLock<ImageF64> = OnceLock::new();
    PICTURE.get_or_init(|| {
        let h = hifreq(Content::ToTheCircle);
        let shot = (tinyplanet_view(), HIFREQ_PLANET);
        stitch(both(&h.frames), &h.rig, UNIT, &shot, RigDebug::Off)
    })
}

/// What the rows read that the CPU makes, each made once for the binary:
/// the pairs, the ideals, the true rig's masks and the CPU's pictures, in
/// the order the rows first read them. `dfish_view_rows` makes them on
/// threads of their own while the GPU draws, and a row that reads one
/// first waits for it.
const MADE_BESIDE: &[fn()] = &[
    || {
        cpu_grid_equirect();
    },
    || {
        ideal_equirect(Pattern::Grid);
    },
    || {
        Scene::Grid.masks();
    },
    || {
        lines();
    },
    || {
        ideal_equirect(Pattern::SeamLines);
    },
    || {
        hifreq(Content::ToTheCircle);
        hifreq(Content::ToTheMask);
        cpu_hifreq_planet();
    },
    || {
        for k in 0..KB_BASES.len() {
            cpu_kb_equirect(k);
        }
    },
];

/// The shader's modes the rows draw in, whose pipelines `dfish_view_rows`
/// builds side by side before the first draw.
const MODES: [DebugMode; 6] = [
    DebugMode::Off,
    DebugMode::ImplicitLod,
    DebugMode::Lens0,
    DebugMode::Lens1,
    DebugMode::Level0,
    DebugMode::LodHeatmap,
];

// What the rows share.

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

/// A render as the CPU's pictures and rung 1's scores take one.
fn f64_image(img: &Image) -> ImageF64 {
    ImageF64 {
        width: img.width,
        height: img.height,
        data: unit(img),
    }
}

/// A pair's render against the CPU reference's picture of it by the CPU
/// rule, as [`Run::cpu_check`] holds a frame's: a PSNR floor calibrated
/// when blessing (3 dB under what it measured, never below 36 dB) and no
/// channel more than 0.1 off. Gives the PSNR and the largest difference.
fn cpu_rule(run: &mut Run<'_>, key: &str, got: &Image, cpu: &ImageF64) -> (f64, f64) {
    let size = (got.width, got.height);
    let got = unit(got);
    let p = psnr_of(size, &got, &cpu.data);
    let ok = run.calibrated(&format!("{key}_psnr_db"), p, CPU_MARGIN_DB, CPU_FLOOR_DB);
    let m = max_diff(&got, &cpu.data);
    let ok = run.at_most(&format!("{key}_max_channel"), m, CPU_MAX_CHANNEL) && ok;
    if !ok {
        run.write_diffs(key, size, &got, &cpu.data);
    }
    (p, m)
}

/// 4.2's "SSIM ≥ blessed − 0.005" against the ideal, more than 12° from
/// the seam and for each lens alone, to 0.0001, and the most a blessing
/// may lower it unasked, half its margin: `tests/instrument.rs`'s `SSIM`
/// floor, whose numbers these are.
const SSIM_MARGIN: f64 = 0.005;
const SSIM_STEPS: f64 = 10_000.0;
const SSIM_DROP: f64 = 0.0025;

/// An SSIM against the ideal held as [`Run::calibrated`] holds a PSNR:
/// a floor calibrated when blessing, [`SSIM_MARGIN`] under what it
/// measured and rounded down to a [`SSIM_STEPS`]th, and read from the
/// manifest otherwise. A blessing that would lower a blessed floor by
/// more than [`SSIM_DROP`] fails, unless it was told it may.
fn calibrated_ssim(run: &mut Run<'_>, key: &str, v: f64) -> bool {
    let tkey = format!("{key}_min");
    let min = if run.blessing() {
        let new = ((v - SSIM_MARGIN) * SSIM_STEPS).floor() / SSIM_STEPS;
        let old = run.blessed(&tkey).ok();
        run.floors.push(format!(
            "{} {tkey}: {} → {new}",
            run.row,
            old.map_or_else(|| "new".into(), |o| o.to_string())
        ));
        if let Some(old) = old
            && new < old - SSIM_DROP
            && !run.lower_floors
        {
            run.fail(format!(
                "blessing would lower {tkey} from {old} to {new} (measured {v:.4}), by more than \
                 {SSIM_DROP}: a render that got worse, or another driver build; to accept it, \
                 bless with {UPDATE}={LOWER}"
            ));
        }
        new
    } else {
        match run.blessed(&tkey) {
            Ok(m) => m,
            Err(e) => {
                run.measure(key, v);
                run.fail(e);
                return false;
            }
        }
    };
    run.at_least(key, v, min)
}

/// The floor `key` is held to this run, as [`Run::calibrated`] or
/// [`calibrated_ssim`] put it among the row's thresholds: blessed, or
/// calibrated by this blessing.
fn floor_of(run: &Run<'_>, key: &str) -> Option<f64> {
    run.thresholds
        .get(&format!("{key}_min"))
        .and_then(Value::as_f64)
}

/// BT.709's luma of a pixel's display values.
fn luma(rgb: &[f64]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// The mean luma of the pixels in `mask`, in 8-bit steps.
fn mean_luma_lsb(mask: &[bool], rgb: &[f64]) -> f64 {
    let (sum, n) = mask
        .iter()
        .zip(rgb.chunks(3))
        .filter(|(m, _)| **m)
        .fold((0.0, 0.0), |(sum, n), (_, px)| (sum + luma(px), n + 1.0));
    sum / n * 255.0
}

/// How many of the pixels in `mask` are black.
fn black_in(mask: &[bool], rgb: &[f64]) -> f64 {
    mask.iter()
        .zip(rgb.chunks(3))
        .filter(|(m, px)| **m && px.iter().all(|v| *v == 0.0))
        .count() as f64
}

/// How many pixels a mask holds.
fn count(mask: &[bool]) -> f64 {
    mask.iter().filter(|m| **m).count() as f64
}

// The rows.

/// 4.2's equidistant control: at least this much worse against the ideal
/// than the true rig, more than 12° from the seam.
const EQUIDISTANT_WORSE_DB: f64 = 6.0;

/// The far `Grid` pair through the 512 × 256 equirect, stored: against
/// the CPU reference by the CPU rule, and against the ideal more than 12°
/// from the seam (the true rig's mask, weighted by cos latitude, as rung
/// 1 scores a stitch), its PSNR to a floor 1 dB under what it measured
/// when blessed and its SSIM to one 0.005 under. Its control is 4.2's,
/// equidistant lenses applied to the Mei pictures
/// (`attribution::equidistant_for`), at least 6 dB worse there; it also
/// misses the SSIM's floor.
fn dfish_equirect_identity(run: &mut Run<'_>) {
    let x5 = x5();
    let pair = x5_source(Scene::Grid.row(), planes(both(grid())), &far());
    let shot = equirect();
    let Some(got) = render(run, &pair, &x5.rig, &shot, DebugMode::Off) else {
        return;
    };
    cpu_rule(run, "cpu", &got, cpu_grid_equirect());
    let mask = &Scene::Grid.masks().past_12;
    let ideal = ideal_equirect(Pattern::Grid);
    let s = rung1::score(&f64_image(&got), ideal, mask, mask);
    run.calibrated("ideal_off_seam_psnr_db", s.psnr, IDEAL_MARGIN_DB, 0.0);
    calibrated_ssim(run, "ideal_off_seam_ssim", s.ssim);
    let equidistant = Rig {
        lenses: x5.rig.lenses.map(|lens| equidistant_for(&lens)),
        ..x5.rig
    };
    let Some(equidistant) = render(run, &pair, &equidistant, &shot, DebugMode::Off) else {
        return;
    };
    let c = rung1::score(&f64_image(&equidistant), ideal, mask, mask);
    run.measure("control_equidistant_ideal_off_seam_psnr_db", c.psnr);
    run.at_least(
        "control_equidistant_worse_db",
        s.psnr - c.psnr,
        EQUIDISTANT_WORSE_DB,
    );
    run.measure("control_equidistant_ideal_off_seam_ssim", c.ssim);
    if let Some(floor) = floor_of(run, "ideal_off_seam_ssim") {
        run.over("control_equidistant_under_ssim_floor", floor - c.ssim, 0.0);
    }
    run.stored_pair(
        "the X5-like camera's far Grid pair stitched through the 512 × 256 equirect",
        &got,
        &pair,
        view_about(&shot.0, shot.1),
    );
}

fn dfish_zenith(run: &mut Run<'_>) {
    pole_twin(run, 2);
}

fn dfish_nadir(run: &mut Run<'_>) {
    pole_twin(run, 3);
}

/// The twin of `rig`: the rig turned by `turn` with its blocks swapped,
/// so that each block holds the other's lens, turned to where its own
/// was, and reads that lens's track.
fn twin_of(rig: &Rig, turn: DQuat) -> Rig {
    let mut lenses = rig.lenses.map(|lens| RigLens {
        lens_from_rig: lens.lens_from_rig * turn.inverse(),
        centre_m: turn * lens.centre_m,
        ..lens
    });
    lenses.swap(0, 1);
    Rig { lenses, ..*rig }
}

/// `twin` with each block reading the track that block read in `camera`:
/// the twin as a stitch draws it that binds each block to the slot of its
/// own index rather than to its lens's track. For a camera whose block b
/// reads track b, as the X5-like camera's do, that stitch draws every
/// other picture of these rows right and only the twin wrong.
fn keeping_tracks(twin: &Rig, camera: &Rig) -> Rig {
    let mut lenses = twin.lenses;
    for (lens, was) in lenses.iter_mut().zip(&camera.lenses) {
        lens.track = was.track;
    }
    Rig { lenses, ..*twin }
}

/// A picture turned 180° in its plane: pixel (i, j) of the result is
/// pixel (w − 1 − i, h − 1 − j) of `rgb`.
fn turned_180(rgb: &[f64]) -> Vec<f64> {
    rgb.chunks(3).rev().flatten().copied().collect()
}

/// A pole of the far `Grid` pair, whole view `k` at 256², against its
/// twin: the true rig turned 180° about the poles' axis (+Y) with its
/// blocks swapped ([`twin_of`]), on the same frames, so that each block
/// draws what the other drew, the picture turned 180° about the pole at
/// the view's centre. Pixel (i, j) of the twin is the camera's (255 − i,
/// 255 − j), from the same texels of the same track, and no pixel may be
/// more than 2/255 apart.
///
/// What the check holds: the seam's circle runs through both poles, so
/// each view is half one lens and half the other, blended across its
/// middle. The half turn about +Y is a symmetry of the stitch's maths,
/// and the swap changes nothing in them: the blend sums over the blocks,
/// so the rig turned without its blocks swapped draws the same picture to
/// the byte (no pixel apart at either pole on lavapipe when this was
/// written). What the swap changes is which block index draws each half
/// and which track each block reads: in the twin block 0 reads track 1,
/// where the camera's block b reads track b. So the check holds that the
/// stitch treats its two blocks alike where they meet at the poles, that
/// it turns with the rig to the byte, and that it binds each block to its
/// own lens's track.
///
/// The control is the last of those mistakes: the twin with each block
/// keeping the track it read in the camera ([`keeping_tracks`]), which is
/// how a stitch that bound each block to the slot of its own index would
/// draw the twin. It must fail the check by over 100 bad pixels, 10.5's
/// bound, and does so by geometry on any driver. Block 0, turned to where
/// lens 0 looked, then reads lens 0's picture through lens 1's model, so
/// near each pixel it shows what the camera itself shows there (about 2°
/// off, the lenses' own small differences), where the twin must show the
/// camera's pixel turned 180° about the pole; and the far `Grid` is not
/// the same under a half turn about +Y (its colour follows the direction,
/// and each pole's disc carries a letter). When blessed it failed by
/// 65 411 of the 65 536 pixels at the zenith and 65 311 at the nadir, up
/// to 255 LSB, 64 049 and 62 751 of them by more than 32 LSB. D32's
/// other-driver rule lets a driver move a pixel of a picture by at most 8
/// LSB, 16 over the two pictures compared, so no conforming driver can
/// bring those within 2/255.
///
/// 10.5's own control, implicit LOD, is measured and not held. Drawn on
/// both the camera and its twin, as `twin_check` draws M1's roll twins,
/// it puts tens of pixels past 2/255 on lavapipe (59 at the zenith and 22
/// at the nadir when blessed, at most 3 LSB) and none on odin's RX 7600
/// under RADV (at most 1 LSB): a fisheye's uv has no wrap, so there is no
/// seam for a 2×2 quad's derivatives to cross, as there is in M1's twins,
/// and the half turn takes quads to quads, so whether the two implicit
/// pictures differ depends on how a driver forms its derivatives, not on
/// the rig. A second control shows the check's reach: the twin drawn with
/// implicit LOD against the camera drawn with the analytic LOD, over 100
/// bad pixels (690 and 255 on lavapipe, 2723 and 753 on RADV). Since the
/// twin is the camera's picture to the byte, that is implicit LOD against
/// analytic on one picture, which shows that 2/255 sees the implicit
/// LOD's departures, not that the twin holds.
///
/// A twin whose pair is rendered afresh through the turned camera can't
/// hold 2/255 at all: synth samples the scene on other texels through
/// each lens's own calibration, and on the 8-bit 4:2:0 frames that is
/// tens of steps on the grid's lines.
fn pole_twin(run: &mut Run<'_>, k: usize) {
    let x5 = x5();
    let pair = x5_source(Scene::Grid.row(), planes(both(grid())), &far());
    let shot = whole_view(k);
    let twin = twin_of(&x5.rig, DQuat::from_rotation_y(PI));
    let by_block = keeping_tracks(&twin, &x5.rig);
    let mut draw = |rig: &Rig, debug: DebugMode| render(run, &pair, rig, &shot, debug);
    let (Some(camera), Some(twin_off), Some(camera_implicit), Some(twin_implicit)) = (
        draw(&x5.rig, DebugMode::Off),
        draw(&twin, DebugMode::Off),
        draw(&x5.rig, DebugMode::ImplicitLod),
        draw(&twin, DebugMode::ImplicitLod),
    ) else {
        return;
    };
    let Some(by_block) = draw(&by_block, DebugMode::Off) else {
        return;
    };
    let camera = unit(&camera);
    let seen = turned_180(&unit(&twin_off));
    let d = bad(shot.1, &camera, &seen, TWIN_THRESHOLD);
    run.measure("bad_pixels_max_lsb", d.max * 255.0);
    if !run.at_most("bad_pixels", d.count_over as f64, 0.0) {
        run.write_diffs("twin", shot.1, &camera, &seen);
    }
    let by_block = turned_180(&unit(&by_block));
    let c = bad(shot.1, &camera, &by_block, TWIN_THRESHOLD);
    run.measure("control_tracks_by_block_bad_pixels_max_lsb", c.max * 255.0);
    if !run.over(
        "control_tracks_by_block_bad_pixels",
        c.count_over as f64,
        CONTROL_BAD_OVER as f64,
    ) {
        run.write_diffs("control_tracks_by_block", shot.1, &camera, &by_block);
    }
    let camera_implicit = unit(&camera_implicit);
    let implicit = turned_180(&unit(&twin_implicit));
    let m = bad(shot.1, &camera_implicit, &implicit, TWIN_THRESHOLD);
    run.measure("implicit_bad_pixels_max_lsb", m.max * 255.0);
    run.measure("implicit_bad_pixels", m.count_over as f64);
    let reach = bad(shot.1, &camera, &implicit, TWIN_THRESHOLD);
    run.measure(
        "control_implicit_against_analytic_bad_pixels_max_lsb",
        reach.max * 255.0,
    );
    run.over(
        "control_implicit_against_analytic_bad_pixels",
        reach.count_over as f64,
        CONTROL_BAD_OVER as f64,
    );
    run.threshold("bad_pixel_over", num(TWIN_THRESHOLD));
    let mut view = view_about(&shot.0, shot.1);
    view["twin"] = json!(
        "the rig turned 180° about +Y with its blocks swapped, on the same frames: pixel (i, j) \
         against the camera's (255 − i, 255 − j)"
    );
    run.end_pair_check(
        "against its twin rig, with each block keeping the camera's track as the control",
        &[&pair],
        vec![view],
    );
}

fn dfish_lens_only_0(run: &mut Run<'_>) {
    lens_only(run, 0);
}

fn dfish_lens_only_1(run: &mut Run<'_>) {
    lens_only(run, 1);
}

/// 4.2's "cx + 2 px": the tune's shift of a lens's centre, in canvas
/// pixels, 5376/768 to a pixel of the track (`Plant::Centre`).
const CENTRE_2_PX: f64 = 14.0;

/// Block `b` of the far `SeamLines` pair alone (debug mode 4 or 5) through
/// the equirect, against the ideal over the true rig's coverage of it:
/// the PSNR over every pixel of it, weighted by cos latitude, to a floor
/// 1 dB under what it measured when blessed, and the SSIM over the
/// windows that lie whole inside it, to one 0.005 under, as rung 1 scores
/// and holds a lens alone. The control moves the lens's centre 2 px along
/// x in the rig, and misses both floors.
fn lens_only(run: &mut Run<'_>, b: usize) {
    let x5 = x5();
    let pair = x5_source(Scene::Lines.row(), planes(both(lines())), &far());
    let shot = equirect();
    let debug = [DebugMode::Lens0, DebugMode::Lens1][b];
    let masks = Scene::Lines.masks();
    let ideal = ideal_equirect(Pattern::SeamLines);
    let score = |img: &Image| {
        rung1::score(
            &f64_image(img),
            ideal,
            &masks.alone[b],
            &masks.alone_windows[b],
        )
    };
    let Some(got) = render(run, &pair, &x5.rig, &shot, debug) else {
        return;
    };
    let s = score(&got);
    run.calibrated("ideal_psnr_db", s.psnr, IDEAL_MARGIN_DB, 0.0);
    calibrated_ssim(run, "ideal_ssim", s.ssim);
    let moved = tuned(|t| t.lenses[b].centre_px = [CENTRE_2_PX, 0.0]);
    let Some(control) = render(run, &pair, &moved, &shot, debug) else {
        return;
    };
    let c = score(&control);
    run.measure("control_cx_2px_ideal_psnr_db", c.psnr);
    run.measure("control_cx_2px_ideal_ssim", c.ssim);
    if let Some(floor) = floor_of(run, "ideal_psnr_db") {
        run.over("control_cx_2px_under_floor_db", floor - c.psnr, 0.0);
    }
    if let Some(floor) = floor_of(run, "ideal_ssim") {
        run.over("control_cx_2px_under_ssim_floor", floor - c.ssim, 0.0);
    }
    run.end_pair_check(
        &format!("block {b} of the far SeamLines pair alone against the ideal over its coverage"),
        &[&pair],
        vec![view_about(&shot.0, shot.1)],
    );
}

/// The tiny planet: stereographic 300°, looking down.
fn tinyplanet_view() -> ViewParams {
    ViewParams::new(ypr(0.0, -90.0, 0.0), lens(Projection::Stereographic, 300.0))
}

/// The pixels of a `size` view of `frames` stitched by `rig` that the
/// stitch minifies by [`IDEAL_MIN_LOD`] levels or more, by the LOD of the
/// lens that weighs more (`reference::rig_lod_map`'s luma before
/// clamping), and shows whole, as [`super::minified`] picks a frame's.
/// A pixel no lens covers has no LOD, and isn't picked.
fn minified_pair(frames: Pair, rig: &Rig, v: &ViewParams, size: (u32, u32)) -> Vec<bool> {
    let lod = reference::rig_lod_map(&ready(frames), rig, v, size).unwrap();
    let dir = ideal_dir(v, size);
    let width = size.0 as usize;
    lod.data
        .chunks(3)
        .enumerate()
        .map(|(k, l)| {
            let (i, j) = ((k % width) as f64, (k / width) as f64);
            let whole = [(i, j), (i + 1.0, j), (i, j + 1.0), (i + 1.0, j + 1.0)]
                .iter()
                .all(|&(x, y)| dir(x, y).is_some());
            whole && l[2] >= IDEAL_MIN_LOD
        })
        .collect()
}

/// The tiny planet's size. The check reads only the pixels 2.5 levels
/// down, a disc about the nadir: at 128² it holds 1267 of them, more
/// than the 1184 the same view holds at `stereo_tinyplanet`'s 256², and
/// the stored picture is 47 kB, where 256²'s moiré, which doesn't
/// compress, takes 197 kB of the 300 kB plan 10.5 gives every new
/// stored PNG.
const HIFREQ_PLANET: (u32, u32) = (128, 128);

/// The `HighFrequency` pair at 384² as a tiny planet on the nadir,
/// stored: against the CPU reference by the CPU rule over the whole
/// picture, and against the ideal over the pixels 2.5 levels down or
/// more, a floor 1 dB under what it measured when blessed and at least
/// 3 dB above the level-0 control, as `stereo_tinyplanet` holds a frame
/// (the CPU rule over the whole picture is the M3.7 review's N5: the
/// ideal's disc holds 1267 of the 16384 pixels, and Tier 2's References
/// mode reads no PNG). The CPU rule's control is the level-0 picture,
/// which aliases: more than 0.1 off the CPU's somewhere.
fn dfish_hifreq_tinyplanet(run: &mut Run<'_>) {
    let h = hifreq(Content::ToTheCircle);
    let pair = h.source("x5_hifreq_384", &far());
    let shot = (tinyplanet_view(), HIFREQ_PLANET);
    let (Some(got), Some(level0)) = (
        render(run, &pair, &h.rig, &shot, DebugMode::Off),
        render(run, &pair, &h.rig, &shot, DebugMode::Level0),
    ) else {
        return;
    };
    let mask = minified_pair(both(&h.frames), &h.rig, &shot.0, HIFREQ_PLANET);
    let ideal = render_view_ideal(
        Pattern::HighFrequency,
        ideal_dir_in(&shot.0, HIFREQ_PLANET, &mask),
        HIFREQ_PLANET,
        IDEAL_N_CHECKER,
    );
    ideal_check(run, &mask, &got, &level0, &ideal);
    let cpu = cpu_hifreq_planet();
    cpu_rule(run, "cpu", &got, cpu);
    let control = unit(&level0);
    run.measure(
        "control_level0_cpu_psnr_db",
        psnr_of(HIFREQ_PLANET, &control, &cpu.data),
    );
    run.over(
        "control_level0_cpu_max_channel",
        max_diff(&control, &cpu.data),
        CPU_MAX_CHANNEL,
    );
    run.stored_pair(
        "the X5-like camera's HighFrequency pair at 384², tiny planet (stereographic 300°, pitch \
         −90)",
        &got,
        &pair,
        view_about(&shot.0, HIFREQ_PLANET),
    );
}

/// How many of `got`'s channels are more than half a level from `want`'s,
/// and the largest difference.
fn levels_off(got: &[f64], want: &[f64]) -> (f64, f64) {
    got.iter()
        .zip(want)
        .fold((0.0, 0.0f64), |(over, worst), (g, w)| {
            let d = sample_diff(*g, *w);
            (over + f64::from(u8::from(d > 0.5)), worst.max(d))
        })
}

/// Block 0's LOD map drawn everywhere its lens has a footprint, whichever
/// lens weighs more: `rig_lod_map`'s three numbers for block 0 alone, from
/// its footprint (`reference::lens_footprint`), on a pair of `frames`
/// whose chroma is half its luma's size each way.
fn block_0_map(frames: Pair, rig: &Rig, v: &ViewParams, size: (u32, u32)) -> Vec<f64> {
    let frame = frames[rig.lenses[0].track];
    let (w, h) = frame.size();
    let levels = [mip_levels(w, h), mip_levels(w.div_ceil(2), h.div_ceil(2))].map(f64::from);
    let mut map = Vec::with_capacity(3 * size.0 as usize * size.1 as usize);
    for j in 0..size.1 {
        for i in 0..size.0 {
            let lods = lens_footprint(rig, 0, v, size, i, j).map_or([0.0; 3], |(gx, gy)| {
                let luma = gx.length().max(gy.length()).max(1e-30).log2();
                [
                    luma.clamp(0.0, levels[0] - 1.0),
                    (luma - 1.0).clamp(0.0, levels[1] - 1.0),
                    luma,
                ]
            });
            map.extend(lods.map(round_f16));
        }
    }
    map
}

/// The far `SeamLines` pair's LOD heat map (debug mode 3, read back
/// linear) through `lod_heatmap`'s fisheye 270°, against the CPU's
/// `reference::rig_lod_map`, lens by lens as each weighs more: no level
/// more than half a level off, D32's LOD rule. Its control is the same
/// rule against block 0's map drawn everywhere.
fn dfish_lod_heatmap(run: &mut Run<'_>) {
    let x5 = x5();
    let frames = both(lines());
    let pair = x5_source(Scene::Lines.row(), planes(frames), &far());
    let v = ViewParams::new(ypr(150.0, 30.0, 0.0), lens(Projection::Fisheye, 270.0));
    let opts = StillOptions {
        debug: DebugMode::LodHeatmap,
        depth: Depth::Linear,
        ..StillOptions::default()
    };
    let Some(got) = run.render_pair(&pair, &x5.rig, pair.gains, &v, SQUARE, opts) else {
        return;
    };
    let got = unit(&got);
    let want = reference::rig_lod_map(&ready(frames), &x5.rig, &v, SQUARE).unwrap();
    let (over, worst) = levels_off(&got, &want.data);
    run.measure("max_level_error", worst);
    run.at_most("levels_off_by_more_than_half", over, 0.0);
    let block_0 = block_0_map(frames, &x5.rig, &v, SQUARE);
    let (control, _) = levels_off(&got, &block_0);
    run.over("control_block_0_levels_off_by_more_than_half", control, 0.0);
    run.end_pair_check(
        "DEBUG_MODE 3's levels against the CPU's rig_lod_map, fisheye 270°",
        &[&pair],
        vec![view_about(&v, SQUARE)],
    );
}

/// The width of the equirect the mask's ring is read in: 9° a pixel,
/// which puts each lens's footprint at the ring 4 levels down or more on
/// the `HighFrequency` pair at 384².
const RING_WIDTH: u32 = 40;

/// The ring's equirects: the 40 × 20 equirect turned a quarter of its
/// pixel at a time about +Y, so that the ring's few pixels in each fall
/// at four places along it.
fn ring_turns() -> [ViewParams; 4] {
    let step = 360.0 / f64::from(RING_WIDTH) / 4.0;
    [0.0, 1.0, 2.0, 3.0]
        .map(|k| ViewParams::equirect(DQuat::from_rotation_y((k * step).to_radians())))
}

/// How far in from the mask's edge the ring reaches, in degrees.
const RING_DEG: f64 = 2.0;

/// The most the ring's mean luma may be from the ideal's, in 8-bit
/// steps.
const RING_LSB: f64 = 4.0;

/// The least LOD at the ring, 10.5's λ ≥ 4: with h = 5° a lens's weight
/// is 0 past 95°, which the mips' bleed from the black past θ_max reaches
/// only at λ ≥ 4.
const RING_MIN_LOD: f64 = 4.0;

/// Block `b`'s ring in a `size` view: the pixels its mask keeps within
/// [`RING_DEG`] of the mask's θ bound at the pixel's own LOD (θ_max less
/// the margin at it), and the least and the greatest of those LODs.
fn ring(rig: &Rig, b: usize, v: &ViewParams, size: (u32, u32)) -> (Vec<bool>, f64, f64) {
    let lens = &rig.lenses[b];
    let (mut least, mut most) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut mask = Vec::with_capacity(size.0 as usize * size.1 as usize);
    for j in 0..size.1 {
        for i in 0..size.0 {
            let c = pixel::centre(i, j);
            let d = v.dir(size, c.x, c.y);
            let held = lens_footprint(rig, b, v, size, i, j).is_some_and(|(gx, gy)| {
                let lod = gx.length().max(gy.length()).max(1e-30).log2();
                let ray = rig.ray(b, d);
                let theta = ray.truncate().length().atan2(ray.z);
                let bound = lens.theta_max
                    - rig.margin_texels * lod.max(0.0).exp2() / lens.texels_per_radian;
                let inside = rig.covers(b, d, lod) && theta >= bound - RING_DEG.to_radians();
                if inside {
                    least = least.min(lod);
                    most = most.max(lod);
                }
                inside
            });
            mask.push(held);
        }
    }
    (mask, least, most)
}

/// The edge of a coverage: the pixels it covers with a pixel beside them
/// (left, right, above or below) that it doesn't, in a picture `width`
/// wide.
fn edge_of(cover: &[bool], width: usize) -> Vec<bool> {
    let height = cover.len() / width;
    (0..cover.len())
        .map(|n| {
            let (x, y) = (n % width, n / width);
            let out = |dx: isize, dy: isize| {
                let (x, y) = (x as isize + dx, y as isize + dy);
                (0..width as isize).contains(&x)
                    && (0..height as isize).contains(&y)
                    && !cover[y as usize * width + x as usize]
            };
            cover[n] && (out(-1, 0) || out(1, 0) || out(0, -1) || out(0, 1))
        })
        .collect()
}

/// `rig` with its mask's frame term defeated: each lens's side, which the
/// term reads, set past its slot's, so that a sample past the picture's
/// right or bottom edge counts as inside (10.5's control, "frame term
/// off"; `tests/reference_rig.rs` defeats it the same way).
fn frame_term_defeated(rig: &Rig) -> Rig {
    let mut loose = *rig;
    for lens in &mut loose.lenses {
        lens.side *= 2;
    }
    loose
}

/// The mask's margin and frame term (10.5), on each lens alone at 384²
/// over `HighFrequency`, where its 1° checker averages to a flat grey and
/// a mip's bleed shows as a step in luma.
///
/// The margin: on the pair whose truth is black past the rig's θ_max
/// (100°), each block alone (debug modes 4 and 5) through a 40 × 20
/// equirect, 9° a pixel, turned a quarter of a pixel at a time four times
/// ([`ring_turns`]), which puts the lens's footprint at the mask's ring 4
/// levels down or more (the least and greatest λ there are recorded): no
/// pixel of the ring black, and the ring's mean luma within 4 LSB of the
/// ideal's, the ring being what the mask keeps within 2° of its θ bound.
/// Its control is the margin at 0, which lets the mips' bleed of the
/// black past θ_max into the ring: a dip of more than 4 LSB there, in
/// each lens's view.
///
/// The frame term: on the pair whose truth carries content to 106°, past
/// the slot's frame, lens 0 alone with θ_max at 104°, past where the
/// frame cuts its circle, through the 512 × 256 equirect: the edge of its
/// coverage within 4 LSB of the ideal's luma, and none of it black. Its
/// control is the frame term off, which takes the lens past its picture's
/// edge, where the sampler repeats the edge's texels: streaks, more than
/// 4 LSB from the ideal on average, outside the lens's coverage.
fn dfish_mask_margin(run: &mut Run<'_>) {
    let masked = hifreq(Content::ToTheMask);
    let ring_pair = masked.source("x5_hifreq_384_black_past_100", &far());
    let size = (RING_WIDTH, RING_WIDTH / 2);
    let no_margin = Rig {
        margin_texels: 0.0,
        ..masked.rig
    };
    for b in 0..2 {
        let debug = [DebugMode::Lens0, DebugMode::Lens1][b];
        // Each turn's ring pixels, the render, the margin-0 control's ring
        // and render, and the ideal over both rings, one turn after another.
        let (mut held, mut got, mut held_0, mut bled, mut ideal) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut least, mut most) = (f64::INFINITY, f64::NEG_INFINITY);
        for turn in ring_turns() {
            let shot = (turn, size);
            let (Some(drawn), Some(drawn_0)) = (
                render(run, &ring_pair, &masked.rig, &shot, debug),
                render(run, &ring_pair, &no_margin, &shot, debug),
            ) else {
                return;
            };
            let (ring_here, lo, hi) = ring(&masked.rig, b, &turn, size);
            let (ring_0, _, _) = ring(&no_margin, b, &turn, size);
            let either: Vec<bool> = ring_here
                .iter()
                .zip(&ring_0)
                .map(|(a, c)| *a || *c)
                .collect();
            let picture = render_view_ideal(
                Pattern::HighFrequency,
                ideal_dir_in(&turn, size, &either),
                size,
                IDEAL_N_CHECKER,
            );
            (least, most) = (least.min(lo), most.max(hi));
            held.extend(ring_here);
            held_0.extend(ring_0);
            got.extend(unit(&drawn));
            bled.extend(unit(&drawn_0));
            ideal.extend(rgb_unit(&picture));
        }
        let key = format!("lens_{b}_ring");
        run.at_least(&format!("{key}_pixels"), count(&held), 1.0);
        run.at_least(&format!("{key}_lod_min"), least, RING_MIN_LOD);
        run.measure(&format!("{key}_lod_max"), most);
        run.at_most(&format!("{key}_black_pixels"), black_in(&held, &got), 0.0);
        let off = mean_luma_lsb(&held, &got) - mean_luma_lsb(&held, &ideal);
        run.at_most(&format!("{key}_luma_off_lsb"), off.abs(), RING_LSB);
        let dip = mean_luma_lsb(&held_0, &ideal) - mean_luma_lsb(&held_0, &bled);
        run.measure(
            &format!("control_margin_0_lens_{b}_ring_pixels"),
            count(&held_0),
        );
        run.over(&format!("control_margin_0_lens_{b}_dip_lsb"), dip, RING_LSB);
    }

    let wide_pair_frames = hifreq(Content::ToTheCircle);
    let mut tune = far();
    tune.lenses[0].theta_max_deg = WIDE_DEG;
    let x5 = x5();
    let wide = sphere_rig(&x5.cal, &x5.planted, &tune, FRONT_TRACK, [HIFREQ_SIDE; 2]).unwrap();
    let wide_pair = wide_pair_frames.source("x5_hifreq_384", &tune);
    let shot = equirect();
    let loose = frame_term_defeated(&wide);
    let (Some(got), Some(streaked)) = (
        render(run, &wide_pair, &wide, &shot, DebugMode::Lens0),
        render(run, &wide_pair, &loose, &shot, DebugMode::Lens0),
    ) else {
        return;
    };
    let (got, streaked) = (unit(&got), unit(&streaked));
    let held = |rig: &Rig| -> Vec<bool> {
        lens_coverage(rig, 0, &shot.0, shot.1)
            .iter()
            .map(|w| *w > 0.0)
            .collect()
    };
    let cover = held(&wide);
    let edge = edge_of(&cover, shot.1.0 as usize);
    let outside: Vec<bool> = held(&loose)
        .iter()
        .zip(&cover)
        .map(|(l, c)| *l && !c)
        .collect();
    let either: Vec<bool> = edge.iter().zip(&outside).map(|(e, o)| *e || *o).collect();
    let ideal = render_view_ideal(
        Pattern::HighFrequency,
        ideal_dir_in(&shot.0, shot.1, &either),
        shot.1,
        IDEAL_N_CHECKER,
    );
    let ideal = rgb_unit(&ideal);
    run.at_least("lens_0_wide_edge_pixels", count(&edge), 1.0);
    run.at_most("lens_0_wide_edge_black_pixels", black_in(&edge, &got), 0.0);
    let off = mean_luma_lsb(&edge, &got) - mean_luma_lsb(&edge, &ideal);
    run.at_most("lens_0_wide_edge_luma_off_lsb", off.abs(), RING_LSB);
    let streaks = outside
        .iter()
        .zip(streaked.chunks(3).zip(ideal.chunks(3)))
        .filter(|(o, _)| **o)
        .map(|(_, (s, i))| (luma(s) - luma(i)).abs() * 255.0)
        .collect::<Vec<f64>>();
    run.measure(
        "control_frame_term_off_pixels_outside",
        streaks.len() as f64,
    );
    let mean = streaks.iter().sum::<f64>() / streaks.len().max(1) as f64;
    run.over("control_frame_term_off_streaks_lsb", mean, RING_LSB);
    let mut ring_view = view_about(&ViewParams::equirect(DQuat::IDENTITY), size);
    ring_view["ring_deg"] = num(RING_DEG);
    ring_view["yaws_deg"] =
        json!(ring_turns().map(|t| num(Ypr::from_quat(t.orientation).yaw.to_degrees())));
    let mut wide_view = view_about(&shot.0, shot.1);
    wide_view["lens_0_theta_max_deg"] = num(WIDE_DEG);
    run.end_pair_check(
        "each lens alone at the mask's ring, minified 4 levels or more, with the margin-0 \
         control; lens 0 alone at θ_max 104° with the frame-term-off control",
        &[&ring_pair, &wide_pair],
        vec![ring_view, wide_view],
    );
}

/// Lens 0's θ_max in the frame term's case, past where its slot's frame
/// cuts its circle.
const WIDE_DEG: f64 = 104.0;

/// The Kannala–Brandt camera through `fs_rig`'s model 1 under each of its
/// four bases, each base its own pair at 256², through the 512 × 256
/// equirect: against the CPU reference by the CPU rule. Its control reads
/// the equidistant camera's lenses as equisolid
/// (`attribution::equisolid_for`), which fails the CPU rule against the
/// CPU's picture by the true rig.
///
/// No pair's rig is built from a calibration: each is `rig_of_truth`'s
/// over `kb_like`'s truth on its base ([`kb_on`]). Its source's
/// calibration hash is the X5-like calibration's, whose extrinsics,
/// centres and planting that truth keeps, and the lenses it replaces
/// them with are its geometry's `kb_truth`.
fn dfish_kb_rig(run: &mut Run<'_>) {
    let x5 = x5();
    let shot = equirect();
    let mut sources = Vec::with_capacity(KB_BASES.len());
    for (k, base) in KB_BASES.iter().enumerate() {
        let kb = kb_on(k);
        let name = format!("kb_{}_seamlines_256", format!("{base:?}").to_lowercase());
        let mut geometry = pair_geometry(&x5.planted, &far(), f64::INFINITY);
        geometry["kb_truth"] = json!({
            "rig": "rig_of_truth over kb_like's truth: the X5-like camera's extrinsics, centres \
                    and planting (its calibration's hash), with these lenses",
            "lenses": kb.lenses,
        });
        let pair = PairSource::new(
            &name,
            [&kb.frames[0], &kb.frames[1]],
            &x5.cal,
            geometry,
            &kb.rig,
            UNIT,
        );
        let Some(got) = render(run, &pair, &kb.rig, &shot, DebugMode::Off) else {
            return;
        };
        let key = format!("cpu_{}", format!("{base:?}").to_lowercase());
        cpu_rule(run, &key, &got, cpu_kb_equirect(k));
        sources.push(pair);
    }
    let kb = kb_on(0);
    let equisolid = Rig {
        lenses: kb.rig.lenses.map(|lens| equisolid_for(&lens)),
        ..kb.rig
    };
    let Some(control) = render(run, &sources[0], &equisolid, &shot, DebugMode::Off) else {
        return;
    };
    let control = unit(&control);
    let cpu = cpu_kb_equirect(0);
    run.measure(
        "control_equisolid_cpu_psnr_db",
        psnr_of(shot.1, &control, &cpu.data),
    );
    run.over(
        "control_equisolid_cpu_max_channel",
        max_diff(&control, &cpu.data),
        CPU_MAX_CHANNEL,
    );
    let sources: Vec<&PairSource<'_>> = sources.iter().collect();
    run.end_pair_check(
        "the Kannala–Brandt camera through fs_rig's model 1 on each base, against the CPU \
         reference",
        &sources,
        vec![view_about(&shot.0, shot.1)],
    );
}
