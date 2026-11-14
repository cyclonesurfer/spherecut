//! Rung 1 of the round trip (M3 plan 4.2) and the attribution matrix
//! (8.3): synth's lens images through the CPU reference's stitch, against
//! the analytic ideal and against what the geometry predicts. One binary,
//! so that rung 1 and the matrix share the renders of
//! `RenderCache::shared()` (7.1).
//!
//! The scene is 4.2's: `SeamLines` and `Grid` at frame 0, each infinitely
//! far and on a sphere of 1.5 m, through synth's X5-like camera at 768² a
//! lens (`x5_like` under `x5_like_planted`) and, for the second rig,
//! `SeamLines` infinitely far through the same camera with Kannala–Brandt
//! lenses (`kb_like`). Each lens's picture is coded as the X5 codes it,
//! and sphere's rig of the same numbers (`common::rig`) stitches the pair
//! at q = 0 or 1/1.5 m⁻¹ into a 512 × 256 equirect and into 90°
//! rectilinear views of 256² on each side seam (±X) and each pole (±Y).
//! Of a view, the stripe that holds the band is rendered, the view's own
//! pixels there, since the band check reads no others ([`view`]). The
//! ideal is `render_view_ideal` of each view's own directions. Every
//! picture is frame 0's, and no check reads a frame number from the
//! scene: `Grid`'s code on the back hemisphere isn't built, and rung 1
//! needs none.
//!
//! What each of the five scenes is held to ([`Scene`]): its stitch more
//! than 12° from the seam and each lens alone, against blessed floors,
//! with a wrong model failing them; and 4.2's band rule on the four
//! views, with lens 1 turned 0.5° failing it on each. The band's rule
//! holds a stitch to the better of its own two lenses, so it sees an
//! error that one lens has and the other hasn't, and none that both
//! share ([`the_bands_hold`]): the near pairs stitched at q = 0 pass it.
//! A lens alone is black past its coverage, so its SSIM is taken over
//! the windows that lie whole inside the coverage
//! ([`Masks::alone_windows`](common::rung1::Masks::alone_windows)).
//!
//! The exposure step is measured on `SeamLines` infinitely far alone:
//! `Grid`'s lines at 1.0 clip under 4.2's +0.5 EV at either depth, and
//! the step leaves a clipped sample out.
//!
//! A pair's mip chains are made once and shared by every stitch and
//! strip of the pair ([`ready`](common::rung1::ready)).
//!
//! A planted error ([`Plant`]) goes into the model that unwraps one
//! rendered pair; depth, motion and gains are rendered again (8.3). Each
//! plant's seam is measured once ([`Plant::seam`]) and shared by the
//! tests.
//!
//! Pixels: 4.2's 0.1 px and 8.3's 0.05 px are the strips' own (3.81 a
//! degree both ways), as the seam's own tests count them; 3.4's rows are
//! the source's at 768² (3.81 a degree across the seam, 6.0 along it).
//!
//! 4.2's "no channel more than 0.1 off the CPU reference" isn't held
//! here, where the pictures are the CPU reference's: it is rung 2's rule,
//! the shader against these pictures (M3.7). Rung 1's camera, far tune,
//! frames and views are `synth::rung`'s (M3.5), and the boundary and
//! `sphere_rig` are in the boundary crate (M3.5, F137): rung 3 and M3.7's
//! rung 2 reach both, so every rung's camera is this one. The scenes
//! ([`Scene`]), the masks ([`Masks`](common::rung1::Masks)), the
//! stitches ([`stitch`]), the ideals ([`ideal_equirect`]) and the
//! scores, with the camera's near scene and its rigs and the
//! Kannala–Brandt camera, are in `common::rung1`, and the attribution
//! ([`Plant`], [`Lands`], [`attribute_to`], [`apart_deg`]) in
//! `common::attribution`: M3.7 lifted them from this file for rung 2, and
//! this file holds them to rung 1's floors, its band rule and the matrix.
//!
//! The floors are blessed by D32's rules, as `goldens.rs` keeps them, into
//! `goldens/instrument.json`: `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p
//! spherecut-render --test instrument` writes it, once every test of the
//! binary has passed and every control has failed, and
//! `SPHERECUT_UPDATE_GOLDENS=lower` lets a floor drop by more than 0.5 dB
//! (for an SSIM floor, 0.0025).
//!
//! Budget (4.1 rule 5, 19.6): 11 s of the 15 s that M3.4's suites share;
//! each test prints its time and the suite's. The share is for four free
//! cores. The binary is bound by the CPU, at about 41 CPU-seconds: about
//! 10.5 s on four cores, 20.6 s on two and 40.5 s on one (DECISIONS D65),
//! so on fewer cores, or beside another build, it passes its share with
//! nothing changed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use serde_json::{Map, Value, json};
use spherecut_render::reference::{ImageF64, RigDebug, srgb_oetf};
use spherecut_render::seam::{BinShift, Signature};
use spherecut_sphere::rig::Rig;
use spherecut_synth::Pattern;
use spherecut_synth::rung::{SIDE, SS, VIEW_SIDE};
use spherecut_testkit::bless;
use spherecut_testkit::control::{at_least, at_most, below, must_fail};

use common::attribution::{
    Attributed, Lands, MISREGISTRATION_PX, Misfit, NCC, Plant, SPEC, Seam, TERM_DEG, apart_deg,
    attribute_to, both_turned, gained, hold_seam, line, pats_v3, scales,
};
use common::budget::{
    A0, A1, ACROSS_PX, ALONG_PX, B1, Budget, C0, C1, CENTRE_ACROSS_DEG, CENTRE_ALONG_DEG, D1,
    SMALL_PX, centres_apart, harmonics, seam_radius, sizes, small, worse,
};
use common::rung1::{
    ALONE, IDEAL_N, OFF_SEAM_DEG, STRIPE, Scene, Score, UNIT, VIEW_KEYS, VIEWS, equirect,
    ideal_equirect, seam_runs_down, ssim, stitch, view, whole_view, x5,
};

// The clock, and what a run blesses.

/// The suite's budget in seconds (the module's doc).
const BUDGET_S: f64 = 11.0;

/// The binary's tests. A blessing is written once each has ended well.
const TESTS: [&str; 7] = [
    "a_blessing_keeps_d32s_rules",
    "rung1_x5_like_rig_meets_the_thresholds",
    "rung1_kb_rig_meets_the_thresholds",
    "rung1_exposure_step_once_matched",
    "rung1_common_mode_control",
    "seam_signature_attributes_planted_errors",
    "error_budget_matches_the_images",
];

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and how
/// long the suite has run, and tells the ledger how the test ended. The
/// tests run side by side, so none of them can time the rest: the last
/// line printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    assert!(TESTS.contains(&name), "{name} isn't among TESTS");
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s of M3.4's budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
        ledger().end(self.name, !std::thread::panicking());
    }
}

/// Runs the parts of a test side by side, and fails as the first of them
/// to fail did.
fn side_by_side(parts: &[&(dyn Fn() + Sync)]) {
    std::thread::scope(|s| {
        let running: Vec<_> = parts.iter().map(|part| s.spawn(part)).collect();
        for part in running {
            if let Err(panic) = part.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
}

/// Where the blessed floors are kept, beside the render goldens'
/// manifest.
const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens");
const MANIFEST: &str = "instrument.json";

/// Set to bless; set to [`LOWER`] to let the blessing lower a floor by
/// more than its [`Floor::drop`]. `goldens.rs`'s variable, read by
/// [`bless`]'s one rule (F055).
const UPDATE: &str = bless::UPDATE_GOLDENS;
const LOWER: &str = bless::LOWER;
const REGENERATE: &str =
    "SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test instrument";

fn blessing() -> bool {
    bless::on(UPDATE)
}

fn may_lower() -> bool {
    bless::lower(UPDATE)
}

/// How a floor is blessed (4.2's thresholds, D32's rules): `margin` below
/// what was measured, rounded down to a `1/steps`, and never lowered by
/// more than `drop` unless the blessing is told it may.
struct Floor {
    margin: f64,
    steps: f64,
    drop: f64,
}

impl Floor {
    /// The floor that a measurement of `v` blesses.
    fn under(&self, v: f64) -> f64 {
        ((v - self.margin) * self.steps).floor() / self.steps
    }

    /// Whether a blessing may move the floor from `old` to `new`: up, or
    /// down by no more than its drop, or anywhere when it `may_lower`.
    fn may_move(&self, old: Option<f64>, new: f64, may_lower: bool) -> bool {
        may_lower || old.is_none_or(|old| new >= old - self.drop)
    }
}

/// "PSNR ≥ the blessed value − 1 dB", to 0.01 dB; D32's 0.5 dB.
const PSNR: Floor = Floor {
    margin: 1.0,
    steps: 100.0,
    drop: 0.5,
};

/// "SSIM ≥ blessed − 0.005", to 0.0001. D32 gives a drop for dB alone;
/// an SSIM floor's is half its margin, as a PSNR floor's is.
const SSIM: Floor = Floor {
    margin: 0.005,
    steps: 10_000.0,
    drop: 0.0025,
};

/// 4.2's margin over the common-mode control, in dB.
const OVER_COMMON_MODE_DB: f64 = 3.0;

/// The manifest as read, or why there is none.
fn manifest() -> &'static Result<Value, String> {
    static READ: OnceLock<Result<Value, String>> = OnceLock::new();
    READ.get_or_init(|| {
        let path = Path::new(GOLDENS).join(MANIFEST);
        std::fs::read_to_string(&path)
            .map_err(|e| format!("no manifest at {} ({e})", path.display()))
            .and_then(|t| {
                serde_json::from_str(&t)
                    .map_err(|e| format!("{} doesn't parse: {e}", path.display()))
            })
    })
}

/// A blessed threshold of a row, from the manifest.
fn blessed(row: &str, key: &str) -> Result<f64, String> {
    let m = manifest().as_ref().map_err(Clone::clone)?;
    m["rows"][row]["thresholds"][key]
        .as_f64()
        .ok_or_else(|| format!("the manifest has no {key} for {row}: bless with `{REGENERATE}`"))
}

/// The floor in force for `key` of `row`, whose true picture measures
/// `v`: the manifest's, or, when blessing, the one this run would write.
fn floor_in_force(row: &str, key: &str, v: f64, floor: &Floor) -> f64 {
    if blessing() {
        floor.under(v)
    } else {
        blessed(row, &format!("{key}_min")).unwrap_or_else(|e| panic!("{e}"))
    }
}

/// A number as the manifest keeps it, to four places.
fn num(v: f64) -> Value {
    json!((v * 1e4).round() / 1e4)
}

/// A control's name as the start of a key of the manifest: its letters
/// and digits in lower case, joined by `_`.
fn slug(name: &str) -> String {
    let words = name.split(|c: char| !c.is_ascii_alphanumeric());
    let words: Vec<String> = words
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    words.join("_")
}

/// One picture's row of the manifest: its floors, and what it measured.
struct Row {
    name: &'static str,
    what: String,
    thresholds: Map<String, Value>,
    measured: Map<String, Value>,
}

impl Row {
    fn new(name: &'static str, what: String) -> Row {
        Row {
            name,
            what,
            thresholds: Map::new(),
            measured: Map::new(),
        }
    }

    fn measure(&mut self, key: &str, v: f64) {
        eprintln!("{}: {key} = {v:.4}", self.name);
        self.measured.insert(key.into(), num(v));
    }

    /// Holds `v` to its floor, and gives the floor back for the row's
    /// controls. A blessing that would lower a blessed floor by more than
    /// its drop fails, unless it was told it may.
    fn floor(&mut self, key: &str, v: f64, floor: &Floor) -> f64 {
        self.measure(key, v);
        let tkey = format!("{key}_min");
        let min = floor_in_force(self.name, key, v, floor);
        if blessing() {
            let old = blessed(self.name, &tkey).ok();
            let was = old.map_or_else(|| "new".into(), |o| o.to_string());
            ledger()
                .floors
                .push(format!("{} {tkey}: {was} → {min}", self.name));
            assert!(
                floor.may_move(old, min, may_lower()),
                "{}: blessing would lower {tkey} from {was} to {min} (measured {v:.4}), by more \
                 than {}: a stitch that got worse; to accept it, bless with {UPDATE}={LOWER}",
                self.name,
                floor.drop
            );
        }
        self.thresholds.insert(tkey, json!(min));
        assert!(
            v >= min,
            "{}: {key} is {v:.4}, below its floor {min}",
            self.name
        );
        min
    }

    /// Hands the row to the ledger, for a blessing to write.
    fn done(self) {
        let row = json!({
            "what": self.what,
            "thresholds": self.thresholds,
            "measured": self.measured,
        });
        ledger().rows.insert(self.name, row);
    }
}

/// What the run would bless, and how its tests have ended.
struct Ledger {
    rows: BTreeMap<&'static str, Value>,
    /// A blessing's floors, old and new.
    floors: Vec<String>,
    /// Each test that has ended, and whether it ended well.
    ended: Vec<(&'static str, bool)>,
}

fn ledger() -> MutexGuard<'static, Ledger> {
    static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
        rows: BTreeMap::new(),
        floors: Vec::new(),
        ended: Vec::new(),
    });
    LEDGER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a blessing does once some of the tests have ended.
#[derive(Debug, PartialEq)]
enum Blessing {
    /// Each of [`TESTS`] ended well: the manifest is written.
    Writes,
    /// These haven't ended.
    Waits(Vec<&'static str>),
    /// These failed, a check or a control: nothing is written.
    Refused(Vec<&'static str>),
}

impl Blessing {
    fn after(ended: &[(&'static str, bool)]) -> Blessing {
        let failed: Vec<&str> = ended.iter().filter(|e| !e.1).map(|e| e.0).collect();
        let waits: Vec<&str> = TESTS
            .into_iter()
            .filter(|t| !ended.iter().any(|e| e.0 == *t))
            .collect();
        if !failed.is_empty() {
            Blessing::Refused(failed)
        } else if !waits.is_empty() {
            Blessing::Waits(waits)
        } else {
            Blessing::Writes
        }
    }
}

impl Ledger {
    /// Notes how a test ended. When blessing, the manifest is written by
    /// the last of [`TESTS`] to end, if every one of them ended well: a
    /// check that failed, a control that passed or a test left out of the
    /// run leaves the manifest as it was.
    fn end(&mut self, name: &'static str, well: bool) {
        self.ended.push((name, well));
        if !blessing() {
            return;
        }
        let said = match Blessing::after(&self.ended) {
            Blessing::Refused(failed) => format!("not blessed: {failed:?} failed\n"),
            Blessing::Waits(waits) => {
                format!("{name} ended well; the blessing waits on {waits:?}\n")
            }
            Blessing::Writes => self.write(),
        };
        // Straight to the terminal: a passing test's eprintln! is
        // captured, and whoever blesses should see the floors move.
        use std::io::Write;
        let _ = std::io::stderr().lock().write_all(said.as_bytes());
    }

    fn write(&self) -> String {
        let manifest = json!({
            "schema": "spherecut.instrument/1",
            "about": "Rung 1 of the stitching round trip (M3 plan 4.2, DECISIONS D65): synth's \
                      lens pictures through the CPU reference's stitch against the analytic \
                      ideal, with each picture's blessed floors and what it measured when \
                      blessed.",
            "regenerate": REGENERATE,
            "date": today(),
            "commit": head_commit(),
            "rows": self.rows,
        });
        let path = Path::new(GOLDENS).join(MANIFEST);
        let text = serde_json::to_string_pretty(&manifest).unwrap() + "\n";
        std::fs::write(&path, text).unwrap();
        format!(
            "blessed {} rows into {}, with these floors (old → new):\n  {}\n",
            self.rows.len(),
            path.display(),
            self.floors.join("\n  ")
        )
    }
}

/// The commit a blessing was made on (the tree may hold changes on top
/// of it), as `goldens.rs` records it.
fn head_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Today in UTC, as YYYY-MM-DD (Hinnant's days-to-civil), as `goldens.rs`
/// dates its rows.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// What rung 1 holds its scenes and plants to. The cameras, their
// pictures, the views, the ideals, the stitches, the scores, the masks
// and the scenes are `common::rung1`'s, and the plants, their seams and
// the attribution `common::attribution`'s (M3.7).

/// Measures the plants' seams ahead of a test's checks, three at a time.
fn warm(plants: &[Plant]) {
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| {
                while let Some(plant) = plants.get(next.fetch_add(1, Ordering::Relaxed)) {
                    plant.seam();
                }
            });
        }
    });
}

/// 8.3's bound on step 2, in strip pixels rms.
const PREDICTED_PX: f64 = 0.05;

/// How many of the 72 bins the prediction holds with the tracks swapped.
/// The field's 3.6° of a1 takes the seam past the estimator's search of
/// ±12 px about φ = 0° and 180°, and `predict_bins` holds a bin only
/// where every point of its patch lies inside the search: it leaves out
/// the 28 bins within 35° of the two, bins 0 to 6, 29 to 42 and 65 to
/// 71. Bin 6, whose mean lies inside the search and whose farthest
/// points lie past it, is among them: the estimator gives an alias for
/// it, 5.4 px from its mean.
const SWAPPED_BINS: usize = 44;

/// How far [`apart_deg`] may lie from its answer where that is known:
/// from 0 under the truth's own model, and from a planted roll's size.
const APART_STILL_DEG: f64 = 1e-6;
const APART_DEG: f64 = 1e-3;

/// 8.3's step 3 on a plant's seam: its measured terms against its own
/// prediction's.
fn attribute(seam: &Seam, lands: Lands) -> Attributed {
    attribute_to(&seam.want, &seam.got, lands)
}

/// The pixels of a whole view `k` that its stripe would hold if the
/// stripe began `first` columns (rows) in.
fn cut(whole: &ImageF64, k: usize, first: u32) -> Vec<f64> {
    let (_, (width, height)) = view(k);
    let (x0, y0) = if seam_runs_down(k) {
        (first, 0)
    } else {
        (0, first)
    };
    let mut data = Vec::with_capacity(3 * (width * height) as usize);
    for y in y0..y0 + height {
        for x in x0..x0 + width {
            data.extend(whole.at(x, y));
        }
    }
    data
}

/// Holds a stripe to be the whole view's own pixels: the true rig's
/// stitch of `scene` through the whole of view `k`, cut to the stripe, is
/// the stripe's picture to the bit. Its control is the cut a pixel over.
fn the_stripe_is_the_views(scene: Scene, k: usize) {
    let rig = scene.rig();
    let whole = stitch(scene.frames(), &rig, UNIT, &whole_view(k), RigDebug::Off);
    let stripe = stitch(scene.frames(), &rig, UNIT, &view(k), RigDebug::Off);
    let first = (VIEW_SIDE - STRIPE) / 2;
    assert!(
        stripe.data == cut(&whole, k, first),
        "{} {}",
        scene.row(),
        VIEWS[k]
    );
    must_fail(
        &format!("the stripe of {} a pixel over", VIEWS[k]),
        stripe.data == cut(&whole, k, first + 1),
    );
}

/// A far X5-like row's words for 4.2's own control of the band.
const SMALL_TURN: &str = "band_*_small_turn_db: the same on a side seam with lens 1 turned 0.13° \
     about +Y, 4.2's own control, which stays over it; band_*_small_turn_costs_db: what that \
     turn costs the stitch's PSNR in the band, held to be more than nothing.";

/// The manifest's keys for a scene's three pictures: the stitch, and
/// each lens alone.
const KEYS: [[&str; 2]; 3] = [
    ["psnr_db", "ssim"],
    ["lens0_psnr_db", "lens0_ssim"],
    ["lens1_psnr_db", "lens1_ssim"],
];

// A scene's words and floors in the manifest, and the band's control on
// it, which this binary alone keeps.
impl Scene {
    fn what(self) -> String {
        let (camera, pattern, at) = match self {
            Scene::Lines => (
                "x5_like under x5_like_planted",
                "SeamLines",
                "infinitely far, q = 0",
            ),
            Scene::Grid => (
                "x5_like under x5_like_planted",
                "Grid",
                "infinitely far, q = 0",
            ),
            Scene::NearLines => (
                "x5_like under x5_like_planted",
                "SeamLines",
                "on a sphere of 1.5 m, q = 1/1.5 per metre",
            ),
            Scene::NearGrid => (
                "x5_like under x5_like_planted",
                "Grid",
                "on a sphere of 1.5 m, q = 1/1.5 per metre",
            ),
            Scene::KbLines => (
                "kb_like of the X5-like camera",
                "SeamLines",
                "infinitely far, q = 0",
            ),
        };
        let control = match self.pattern() {
            Pattern::Grid => {
                "band_*_yawed_db: the same with lens 1 yawed 0.5°, which must come under it on \
                 a side seam and is recorded on a pole, where the yaw turns the graticule about \
                 its own pole; band_*_rolled_db: with lens 1 rolled 0.5°, which must come under \
                 it on a pole"
            }
            _ => {
                "band_*_yawed_db: the same with lens 1 yawed 0.5°, which must come under it on \
                 each"
            }
        };
        // What the row measures beside the floors and the band.
        let more = match self {
            Scene::Lines => format!(
                " {SMALL_TURN} lens*_edge_windows_ssim: the ideal, black where the lens covers \
                 nothing, against the ideal over the lens's whole coverage, which must come \
                 under 1; over the whole windows it is 1."
            ),
            Scene::Grid => format!(" {SMALL_TURN}"),
            Scene::NearLines | Scene::NearGrid => " band_pos_x_q_0_db: the pair stitched at q = \
                 0, which passes the band's rule, both lenses being wrong alike; \
                 band_pos_x_q_0_costs_db: what that costs the stitch's own PSNR in the band."
                .to_owned(),
            Scene::KbLines => String::new(),
        };
        format!(
            "{pattern} at frame 0, {at}, through {camera} at {SIDE}² a lens ({SS}² samples a \
             pixel), stitched by render_rig into a 512x256 equirect and held to \
             render_view_ideal ({IDEAL_N}² samples a pixel), weighted by cos(latitude): more \
             than {OFF_SEAM_DEG}° from the seam, and each lens alone over its own coverage, \
             its SSIM over the windows that lie whole inside it. band_*_db: in 90° rectilinear \
             views of {VIEW_SIDE}² on each side seam and each pole, where both lenses' weights \
             are above 0, the stitch's PSNR less the better lens's alone, in dB, held to \
             {BAND_DB} dB or more; {control}. The band's rule sees an error that one lens has \
             and the other hasn't, and none that both share.{more}"
        )
    }

    /// Whether lens 1 yawed 0.5° is the band's control on view `k`: on a
    /// side seam always, and on a pole for `SeamLines`, whose checker the
    /// yaw moves across 45° of the seam to either side of the pole. On
    /// `Grid` it isn't: the yaw turns the graticule about its own pole,
    /// which moves the meridians far from the pole and leaves the
    /// parallels, and the band on a pole loses 0.1 dB to 1.3 dB to it.
    /// There the control is the roll, which takes the seam across the
    /// meridians at the pole.
    fn yaw_fails(self, k: usize) -> bool {
        seam_runs_down(k) || self.pattern() != Pattern::Grid
    }

    /// The floors in force for the scene's three pictures.
    fn floors(self) -> [Score; 3] {
        let truth = self.truth();
        std::array::from_fn(|i| Score {
            psnr: floor_in_force(self.row(), KEYS[i][0], truth[i].psnr, &PSNR),
            ssim: floor_in_force(self.row(), KEYS[i][1], truth[i].ssim, &SSIM),
        })
    }

    /// Holds the true rig's pictures to their floors (4.2's thresholds),
    /// and gives the scene's row, for its controls' numbers, and the
    /// floors.
    fn hold(self) -> (Row, [Score; 3]) {
        let mut row = Row::new(self.row(), self.what());
        let truth = self.truth();
        let floors = std::array::from_fn(|i| Score {
            psnr: row.floor(KEYS[i][0], truth[i].psnr, &PSNR),
            ssim: row.floor(KEYS[i][1], truth[i].ssim, &SSIM),
        });
        (row, floors)
    }
}

/// How far, in dB, a stitch lies above the better of its two lenses
/// alone, from the band's three PSNRs: the check is that it is at least
/// [`BAND_DB`].
fn over_the_better([stitched, first, second]: [f64; 3]) -> f64 {
    stitched - first.max(second)
}

/// 4.2's "at most 1 dB below the better of the two lenses alone".
const BAND_DB: f64 = -1.0;

// The blessing.

/// D32's rules as this binary keeps them: the floor a measurement
/// blesses, the drop a blessing refuses unless it is told it may lower,
/// and a manifest written only once every test has ended well.
#[test]
fn a_blessing_keeps_d32s_rules() {
    let _t = timed("a_blessing_keeps_d32s_rules");
    // A floor: the margin below, rounded down to a step.
    assert_eq!(PSNR.under(32.5009), 31.5);
    assert_eq!(SSIM.under(0.95337), 0.9483);

    // A floor may rise, and drop by half its margin; further, only when
    // the blessing is told it may.
    assert!(PSNR.may_move(None, 31.5, false));
    assert!(PSNR.may_move(Some(31.5), 33.0, false));
    assert!(PSNR.may_move(Some(31.5), 31.0, false));
    must_fail(
        "a PSNR floor lowered by 0.51 dB",
        PSNR.may_move(Some(31.5), 30.99, false),
    );
    assert!(PSNR.may_move(Some(31.5), 30.99, true));
    must_fail(
        "an SSIM floor lowered by 0.0026",
        SSIM.may_move(Some(0.9483), 0.9457, false),
    );

    // The manifest is written once each test has ended well.
    let all: Vec<(&str, bool)> = TESTS.into_iter().map(|t| (t, true)).collect();
    assert_eq!(Blessing::after(&all), Blessing::Writes);
    assert_eq!(
        Blessing::after(&all[1..]),
        Blessing::Waits(vec![TESTS[0]]),
        "a test left out of the run"
    );
    let mut one_failed = all.clone();
    one_failed[2].1 = false;
    assert_eq!(
        Blessing::after(&one_failed),
        Blessing::Refused(vec![TESTS[2]]),
        "a check that failed, or a control that passed"
    );
    must_fail(
        "a blessing with a test failed",
        Blessing::after(&one_failed) == Blessing::Writes,
    );
}

// Rung 1.

/// Rung 1 on the X5-like rig: the stitch against the ideal within 4.2's
/// thresholds, away from the seam, each lens alone and in the band, with
/// each geometric control failing.
///
/// Two of 4.2's pairings don't hold as the plan words them, and the test
/// says what holds instead:
///
/// - **The band check's control.** Lens 1 turned 0.13° about +Y costs
///   the band a few tenths of a dB against the better lens, not the
///   3.6 dB of the plan's 1-D model: at the views' 0.35° a pixel a lens
///   alone already scores about 25 dB on the 1° checker, and half a
///   source pixel adds little to that. The check's control is lens 1
///   yawed 0.5°, which fails it by 4 dB or more on the side seams and,
///   on `SeamLines`, by 2 dB on the poles; on `Grid`'s poles, where the
///   yaw turns the graticule about its own pole, it is lens 1 rolled
///   0.5° ([`the_bands_hold`]). The 0.13° turn is held to lower the
///   band's PSNR, and fails the misregistration.
/// - **Median NCC under 0.5.** The turn left out doesn't collapse the
///   NCC on `SeamLines`, whose checker repeats every 2° inside the ±3°
///   search: the strips lock a period away, at the search's edge, with a
///   median NCC of 0.97. Nor would it on `Grid`, whose graticule the half
///   turn maps onto itself (0.998 over 58 bins when this was written).
///   It fails the misregistration by 16 px and the comparison with the
///   ideal. The NCC check's control is track 1 showing another scene.
#[test]
fn rung1_x5_like_rig_meets_the_thresholds() {
    let _t = timed("rung1_x5_like_rig_meets_the_thresholds");
    side_by_side(&[
        &|| warm(&[Plant::Truth, Plant::Near, Plant::OtherScene, Plant::NoTurn]),
        &lines_meet_the_thresholds,
        &grid_meets_the_thresholds,
        &|| near_meets_the_thresholds(Scene::NearLines),
        &|| near_meets_the_thresholds(Scene::NearGrid),
    ]);
}

/// A pattern through the X5-like rig on a sphere of 1.5 m, stitched at
/// q = 1/1.5 m⁻¹: the floors, with the pair stitched at q = 0 failing
/// them, and the band on the four views, with lens 1 turned 0.5° at that
/// distance failing it.
///
/// The stitch at q = 0 is no control for the band: both lenses are wrong
/// by the same parallax, each as bad alone as the stitch, and the rule
/// holds the stitch to the better of them ([`the_bands_hold`]). Its
/// number on +X goes in the row, held to pass the rule that it can't
/// fail.
fn near_meets_the_thresholds(scene: Scene) {
    let (mut row, floors) = scene.hold();
    let at_infinity = Plant::NearAtInfinity.rig();
    let name = "q = 0 against R = 1.5 m";
    fails_the_floors(&mut row, scene, &floors, name, &at_infinity);
    fails_alone(&mut row, scene, &floors, name, &at_infinity);
    the_bands_hold(&mut row, scene, None);
    let [stitched, first, second] = scene.band_scores(&at_infinity, 0);
    let over = over_the_better([stitched, first, second]);
    row.measure("band_pos_x_q_0_db", over);
    row.measure("band_pos_x_q_0_costs_db", scene.band_truth(0)[0] - stitched);
    assert!(
        over >= BAND_DB && stitched < scene.band_truth(0)[0] - 3.0,
        "{}: at q = 0 the stitch is {stitched} dB in the band on +X, {over} dB over its \
         better lens, and the true rig's {} dB",
        scene.row(),
        scene.band_truth(0)[0]
    );
    row.done();
}

/// A control on a scene's floors away from the seam: `model`'s stitch
/// must fall below both. Its numbers go in the scene's row.
fn fails_the_floors(row: &mut Row, scene: Scene, floors: &[Score; 3], name: &str, model: &Rig) {
    let got = scene.stitched(model);
    let key = slug(name);
    row.measure(&format!("{key}_psnr_db"), got.psnr);
    row.measure(&format!("{key}_ssim"), got.ssim);
    let what = format!("{name}, {}", scene.row());
    must_fail(&format!("{what}, PSNR"), at_least(got.psnr, floors[0].psnr));
    must_fail(&format!("{what}, SSIM"), at_least(got.ssim, floors[0].ssim));
}

/// A control on a scene's floors for a lens alone: `model`'s lens 1 must
/// fall below both.
fn fails_alone(row: &mut Row, scene: Scene, floors: &[Score; 3], name: &str, model: &Rig) {
    let got = scene.alone(model, 1);
    let key = slug(name);
    row.measure(&format!("{key}_lens1_psnr_db"), got.psnr);
    row.measure(&format!("{key}_lens1_ssim"), got.ssim);
    let what = format!("{name}, {}, lens 1 alone", scene.row());
    must_fail(&format!("{what}, PSNR"), at_least(got.psnr, floors[2].psnr));
    must_fail(&format!("{what}, SSIM"), at_least(got.ssim, floors[2].ssim));
}

/// The lens-alone SSIM counts no window that reaches past the lens's
/// coverage, where a lens alone is black: the ideal with every pixel the
/// lens doesn't cover made black scores exactly 1 against the ideal over
/// the whole windows
/// ([`Masks::alone_windows`](common::rung1::Masks::alone_windows)). Its
/// control is the same pair over the lens's whole coverage, as the SSIM
/// was taken before, whose windows at the edge hold the black and bring
/// it under 1.
fn the_windows_lie_inside(row: &mut Row, scene: Scene) {
    let (masks, want) = (scene.masks(), ideal_equirect(scene.pattern()));
    let (_, (width, height)) = equirect();
    let size = (width as usize, height as usize);
    for b in 0..2 {
        let cover = &masks.alone[b];
        let blacked: Vec<f64> = want
            .chunks_exact(3)
            .zip(cover)
            .flat_map(|(px, w)| px.iter().map(move |v| if *w > 0.0 { *v } else { 0.0 }))
            .collect();
        let whole = ssim(&blacked, want, &masks.alone_windows[b], size);
        assert!((whole - 1.0).abs() < 1e-12, "lens {b}: {whole}");
        let edge = must_fail(
            &format!("lens {b}'s coverage with its edge windows, the blacked ideal's SSIM"),
            at_least(ssim(&blacked, want, cover, size), 1.0 - 1e-12),
        );
        row.measure(&format!("lens{b}_edge_windows_ssim"), edge);
        let weight = |mask: &[f64]| mask.iter().sum::<f64>();
        eprintln!(
            "{}, lens {b} alone: the windows that reach past its coverage held {:.2} % of the \
             SSIM's weight; the ideal blacked past the coverage scores {edge:.4} with them and \
             {whole} without",
            scene.row(),
            100.0 * (1.0 - weight(&masks.alone_windows[b]) / weight(cover))
        );
    }
}

/// 4.2's band check on the true rig's view `k` of a scene.
fn the_band_holds(row: &mut Row, scene: Scene, k: usize) {
    let over = over_the_better(*scene.band_truth(k));
    row.measure(&format!("band_{}_db", VIEW_KEYS[k]), over);
    assert!(over >= BAND_DB, "{} {}: {over} dB", scene.row(), VIEWS[k]);
}

/// The band check on each side seam and each pole of a scene, with its
/// control on each: the scene's rig with lens 1 turned 0.5°
/// ([`Scene::turned`]), which must fail it. The control is the yaw
/// wherever the yaw fails the rule ([`Scene::yaw_fails`]). A yaw is a
/// turn about +Y, or nearly, so it leaves the poles themselves where
/// they are; a pole's view holds 45° of the seam to either side of its
/// pole, where the turn moves a point of the seam by 0.5° × the sine of
/// its angle from the pole, and on `SeamLines` that fails the rule too,
/// by less than on a side seam. On `Grid`'s poles the yaw's number is
/// recorded and the control is the roll, which moves the seam along
/// itself everywhere.
///
/// `small` is the far X5-like rig with lens 1 turned 0.13° about +Y, 4.2's
/// own control for this check, which doesn't fail it and is held on the
/// side seams to lower the band's PSNR (rung 1's doc).
///
/// What the rule can see: the stitch is held to the better of its own two
/// lenses, so an error that one lens has shows, the other lens staying
/// right, and an error that both share doesn't. The near pairs stitched
/// at q = 0 pass it with the stitch several dB worse in the band
/// ([`near_meets_the_thresholds`]), and so does the common-mode control
/// (`rung1_common_mode_control`). The floors away from the seam and the
/// seam's own checks hold those; nothing that reads this rule may take
/// it for a check of an error both lenses share.
fn the_bands_hold(row: &mut Row, scene: Scene, small: Option<&Rig>) {
    let (yawed, rolled) = (scene.turned(0), scene.turned(2));
    for (k, name) in VIEWS.into_iter().enumerate() {
        the_band_holds(row, scene, k);
        let key = VIEW_KEYS[k];
        let over = over_the_better(scene.band_scores_lens_1(&yawed, k));
        row.measure(&format!("band_{key}_yawed_db"), over);
        if scene.yaw_fails(k) {
            must_fail(
                &format!("lens 1 yawed 0.5°, {}, the band on {name}", scene.row()),
                at_least(over, BAND_DB),
            );
        } else {
            let over = over_the_better(scene.band_scores_lens_1(&rolled, k));
            row.measure(&format!("band_{key}_rolled_db"), over);
            must_fail(
                &format!("lens 1 rolled 0.5°, {}, the band on {name}", scene.row()),
                at_least(over, BAND_DB),
            );
        }
        let Some(small) = small.filter(|_| seam_runs_down(k)) else {
            continue;
        };
        let with_turn = scene.band_scores_lens_1(small, k);
        let costs = scene.band_truth(k)[0] - with_turn[0];
        row.measure(
            &format!("band_{key}_small_turn_db"),
            over_the_better(with_turn),
        );
        row.measure(&format!("band_{key}_small_turn_costs_db"), costs);
        must_fail(
            &format!(
                "lens 1 turned 0.13° about +Y, {}, the band's PSNR on {name}",
                scene.row()
            ),
            at_least(with_turn[0], scene.band_truth(k)[0]),
        );
    }
}

/// `SeamLines` through the X5-like rig, infinitely far.
fn lines_meet_the_thresholds() {
    let scene = Scene::Lines;
    let (mut row, floors) = scene.hold();
    for (name, plant) in [
        ("equidistant applied to Mei data", Plant::Equidistant),
        ("the turn left out", Plant::NoTurn),
        ("the tracks swapped", Plant::Swapped),
    ] {
        fails_the_floors(&mut row, scene, &floors, name, &plant.rig());
    }
    fails_alone(&mut row, scene, &floors, "cx + 2 px", &Plant::Centre.rig());
    the_windows_lie_inside(&mut row, scene);

    // In the band, on each seam and each pole; a stripe of a view is
    // the view's own pixels, on a side seam and on a pole.
    the_bands_hold(&mut row, scene, Some(&Plant::SmallTurn.rig()));
    the_stripe_is_the_views(scene, 0);
    the_stripe_is_the_views(scene, 2);

    // The seam: the strips registered, far and near.
    hold_seam("the X5-like rig", &Plant::Truth.seam());
    hold_seam("the X5-like rig at 1.5 m", &Plant::Near.seam());
    let ncc = must_fail(
        "track 1 showing another scene, the median NCC",
        at_least(Plant::OtherScene.seam().own.median_ncc, NCC),
    );
    row.measure("other_scene_median_ncc", ncc);
    // What the plan pairs the NCC with: no collapse, and 16 px.
    let no_turn = Plant::NoTurn.seam();
    row.measure("the_turn_left_out_median_ncc", no_turn.own.median_ncc);
    row.measure(
        "the_turn_left_out_misregistration_px",
        no_turn.misregistration_px,
    );
    row.done();
}

/// `Grid` through the X5-like rig, infinitely far.
fn grid_meets_the_thresholds() {
    let scene = Scene::Grid;
    let (mut row, floors) = scene.hold();
    let equidistant = Plant::Equidistant.rig();
    fails_the_floors(
        &mut row,
        scene,
        &floors,
        "equidistant applied to Mei data",
        &equidistant,
    );
    fails_alone(&mut row, scene, &floors, "cx + 2 px", &Plant::Centre.rig());
    the_bands_hold(&mut row, scene, Some(&Plant::SmallTurn.rig()));
    row.done();
}

/// Rung 1 on the Kannala–Brandt rig, which shows that the source path
/// isn't Mei's alone.
#[test]
fn rung1_kb_rig_meets_the_thresholds() {
    let _t = timed("rung1_kb_rig_meets_the_thresholds");
    let scene = Scene::KbLines;
    let (mut row, floors) = scene.hold();

    // In the band, on each seam and each pole, with lens 1 turned 0.5°
    // about +Y failing it on each.
    the_bands_hold(&mut row, scene, None);

    // The seam.
    hold_seam("the Kannala–Brandt rig", &Plant::Kb.seam());

    // Control: the lenses read as equisolid.
    let equisolid = Plant::KbEquisolid.rig();
    fails_the_floors(
        &mut row,
        scene,
        &floors,
        "equidistant read as equisolid",
        &equisolid,
    );
    fails_alone(
        &mut row,
        scene,
        &floors,
        "equidistant read as equisolid",
        &equisolid,
    );
    let seam = Plant::KbEquisolid.seam();
    let px = must_fail(
        "equidistant read as equisolid, the misregistration",
        at_most(seam.misregistration_px, MISREGISTRATION_PX),
    );
    row.measure("equidistant_read_as_equisolid_misregistration_px", px);
    row.done();
}

/// With lens 1 at +0.5 EV and +5 % red, the step across the seam is under
/// 1 LSB once the measured gains are applied; with the gains off it
/// isn't.
///
/// The step is what a switch from lens 0 to lens 1 shows: the mean, over
/// the equirect's pixels that both lenses cover, of lens 1 alone less
/// lens 0 alone (10.3's debug modes 5 and 4), each with its gain, in 8-bit
/// steps of the display's light, for each channel. The gains are
/// `render_rig`'s, lens 1's the inverse of the signature's log gains. A
/// pixel is left out where either lens's picture, at unit gain, is above
/// 0.95 in linear light, 8.2's bound for a clipped sample; on `SeamLines`
/// no pixel is.
///
/// The signature's blue gain lies about 1 % above the truth's (0.356 for
/// 0.347), and it is the coded picture's: at mid grey +5 % red moves an
/// 8-bit Cb by 0.3 of a code, which rounds away, so the picture's blue
/// rises with its luma, by about 1 %. The step is smaller under the
/// measured gains than under the truth's own.
#[test]
fn rung1_exposure_step_once_matched() {
    let _t = timed("rung1_exposure_step_once_matched");
    let x5 = x5();
    let seam = Plant::Gains.seam();
    let frames = Plant::Gains.setup().frames;

    // The signature's log gains against the truth's.
    let planted = gained().lenses[1].gain.map(f64::ln);
    let measured = seam.own.log_gain;
    let off = |got: [f64; 3]| (0..3).map(|c| (got[c] - planted[c]).abs()).fold(0.0, worse);
    eprintln!(
        "log gains: measured {measured:.4?}, planted {planted:.4?}, {:.4} off at most",
        off(measured)
    );
    assert!(off(measured) <= LOG_GAIN, "{measured:?} for {planted:?}");
    must_fail(
        "the still pair's log gains for the planted ones",
        at_most(off(Plant::Truth.seam().own.log_gain), LOG_GAIN),
    );
    // Gains leave the geometry alone.
    hold_seam("lens 1 at +0.5 EV and +5 % red", &seam);

    // The step, in 8-bit steps, by channel.
    let shot = equirect();
    let overlap = &Scene::Lines.masks().overlap;
    let alone = |b: usize, gains| stitch(frames, &x5.rig, gains, &shot, ALONE[b]);
    let unmatched = [alone(0, UNIT), alone(1, UNIT)];
    let clip = srgb_oetf(0.95);
    let kept: Vec<bool> = (0..overlap.len())
        .map(|n| {
            let clips = |image: &ImageF64| image.data[3 * n..3 * n + 3].iter().any(|v| *v > clip);
            overlap[n] > 0.0 && !unmatched.iter().any(clips)
        })
        .collect();
    let covered = overlap.iter().filter(|w| **w > 0.0).count();
    let count = kept.iter().filter(|k| **k).count();
    eprintln!(
        "the step is over {count} pixels; {} clip and are left out",
        covered - count
    );
    assert!(count > 10_000, "{count}");
    let step = |lens_1: &ImageF64| -> f64 {
        let mut sum = [0.0; 3];
        for n in (0..kept.len()).filter(|&n| kept[n]) {
            for (c, sum) in sum.iter_mut().enumerate() {
                *sum += lens_1.data[3 * n + c] - unmatched[0].data[3 * n + c];
            }
        }
        let by_channel = sum.map(|s| s / count as f64 * 255.0);
        eprintln!("    the step by channel, in LSB: {by_channel:+.3?}");
        by_channel.iter().map(|s| s.abs()).fold(0.0, worse)
    };
    let gains_of = |log_gain: [f64; 3]| [[1.0; 3], log_gain.map(|l| (-l).exp())];
    let matched = step(&alone(1, gains_of(measured)));
    let by_truth = step(&alone(1, gains_of(planted)));
    eprintln!(
        "the step across the seam: {matched:.3} LSB once matched by the signature's gains, \
         {by_truth:.3} LSB by the truth's"
    );
    assert!(matched < STEP_LSB, "{matched} LSB");
    let gains_off = must_fail("gains off, the step", below(step(&unmatched[1]), STEP_LSB));
    eprintln!("control, gains off: a step of {gains_off:.2} LSB");
    assert!(gains_off >= 8.0, "{gains_off} LSB");
}

/// 4.2's bound on the step across the seam once matched, in 8-bit steps.
const STEP_LSB: f64 = 1.0;

/// How far a measured log gain may lie from the planted one: what a step
/// of 1 LSB is at the middle of the display's range, where 1/153 of the
/// display's light is 1.4 % of the linear.
const LOG_GAIN: f64 = 0.015;

/// Both lenses turned 0.5° about +Y pass every seam check and fail the
/// comparison with the ideal: the common-mode control.
///
/// 4.2 asks the true stitch to score 3 dB over this control, so that a
/// slip common to both lenses shows even where it was there at the
/// bless. On `Grid`, whose lines the turn crosses, it does, for the
/// stitch and for each lens alone. The rule's own control is a stitch
/// that slips 1° already: it scores 1.1 dB over the control made from
/// it, the same 0.5° again. One that slips 0.5° already scores 2.96 dB
/// over its own, under the rule by too little to rest a control on, so
/// that number is recorded and not held: on `Grid` the rule sees a slip
/// from about 0.5° on.
///
/// On `SeamLines` more than 12° from the seam the true stitch is 2.9 dB
/// over the control, and that margin is recorded and not held: the 1°
/// checker reaches 15° from the seam, and at the equirect's 0.70° a
/// pixel it sets the error whatever the registration. A stitch that
/// slips 0.5° already scores 2.8 dB over its own control there, 0.1 dB
/// from the true stitch's margin, so no threshold tells the two apart.
/// With the mask at 15° the margins are 4.0 dB and 3.3 dB, and 4.2's
/// 3 dB passes both (a probe's numbers when this was written). On
/// `SeamLines` the control is held to fail the blessed floors.
#[test]
fn rung1_common_mode_control() {
    let _t = timed("rung1_common_mode_control");
    let slipped = Plant::CommonMode.rig();
    let mut row = Row::new(
        "x5_common_mode",
        format!(
            "Both lenses of x5_like turned 0.5° about +Y, 4.2's common-mode control, which the \
             seam can't see: how far apart its strips lie, its band on SeamLines, and its \
             stitches of SeamLines and of Grid against the ideal more than {OFF_SEAM_DEG}° from \
             the seam, each held to fail that picture's floors. *_over_db: the true rig's PSNR \
             less the control's, held to {OVER_COMMON_MODE_DB} dB on Grid and recorded on \
             SeamLines. grid_slipping_*_over_db: a stitch of Grid that slips so many degrees \
             already, less the control made from it, 0.5° further; the one at 1° is held to \
             come under {OVER_COMMON_MODE_DB} dB"
        ),
    );

    // Every seam check passes.
    let seam = Plant::CommonMode.seam();
    hold_seam("both lenses turned 0.5° about +Y", &seam);
    row.measure("misregistration_px", seam.misregistration_px);
    for k in [0, 2] {
        let over = over_the_better(Scene::Lines.band_scores(&slipped, k));
        row.measure(&format!("seamlines_band_{}_db", VIEW_KEYS[k]), over);
        assert!(over >= BAND_DB, "{}: {over} dB", VIEWS[k]);
    }

    // The comparison with the ideal fails: each picture's floors.
    let mut fails_the_ideal = |scene: Scene, key: &str| {
        let (truth, floors) = (scene.truth(), scene.floors());
        let got = scene.stitched(&slipped);
        row.measure(&format!("{key}_psnr_db"), got.psnr);
        row.measure(&format!("{key}_ssim"), got.ssim);
        let what = format!("both lenses turned 0.5° about +Y, {}", scene.row());
        must_fail(&format!("{what}, PSNR"), at_least(got.psnr, floors[0].psnr));
        must_fail(&format!("{what}, SSIM"), at_least(got.ssim, floors[0].ssim));
        let over = truth[0].psnr - got.psnr;
        row.measure(&format!("{key}_over_db"), over);
        (got, over)
    };
    fails_the_ideal(Scene::Lines, "seamlines");
    let (once, over) = fails_the_ideal(Scene::Grid, "grid");

    // The margin, on the lines the turn crosses: the stitch, and each
    // lens alone.
    let scene = Scene::Grid;
    assert!(
        over >= OVER_COMMON_MODE_DB,
        "{}: {over} dB over",
        scene.row()
    );
    for b in 0..2 {
        let got = scene.alone(&slipped, b);
        let over = scene.truth()[1 + b].psnr - got.psnr;
        row.measure(&format!("grid_lens{b}_over_db"), over);
        assert!(over >= OVER_COMMON_MODE_DB, "lens {b}: {over} dB over");
    }

    // The margin's own control: a stitch that slips already, over the
    // control made from it, the same 0.5° again.
    let slipping = |deg: f64| scene.stitched(&both_turned(deg)).psnr;
    let (by_1, by_1_5) = (slipping(1.0), slipping(1.5));
    row.measure("grid_slipping_0_5_over_db", once.psnr - by_1);
    let over = must_fail(
        "a stitch that slips 1° already, over its own common-mode control",
        at_least(by_1 - by_1_5, OVER_COMMON_MODE_DB),
    );
    row.measure("grid_slipping_1_over_db", over);
    row.done();
}

// The matrix.

/// 8.3's matrix: each planted error is predicted point by point, measured
/// from the images within 0.05 px rms of the prediction, and lands on the
/// term the prediction names, with the other terms under their bound.
///
/// Step 3 has a control for each of its rules ([`Misfit`]), each held to
/// break that rule and no earlier one: a field read as no shift, and as
/// a term the prediction doesn't name; the yaw's measured terms for the
/// pitch's prediction, which land on another term; a centre offset read
/// as a tilt alone, whose d1 is over the bound; a centre offset for the
/// prediction of a V6 reading that lands on a1 too, whose coupled terms
/// come at other ratios; and the fields past the search, which leave no
/// term measured.
///
/// The two depth rows name d1 with a0. The plan has parallax move the
/// seam across itself alone (3.3, 8.2), which holds for lenses whose
/// centres lie on one line along the lens axis. `x5_like`'s lie 4.30 mm
/// apart in the seam's plane (Pat's V3 tx of −4.46 mm, less what lens
/// 0's yaw brings in from tz), so at 1.5 m the seam moves 0.164° along
/// itself too, a first harmonic on d1 at −0.134 of a0, at every depth.
/// The rows hold d1 to that ratio, and not to the 15 % that it comes
/// within 1.6 points of.
///
/// Three controls plant a field larger than the strips' ±3° search, so
/// the prediction holds no bin and steps 2 and 3 have nothing to hold:
/// equidistant applied to Mei data, the turn left out, and the
/// Kannala–Brandt lenses read as equisolid. The plan attributes them by
/// an NCC collapse, which these scenes don't give (rung 1's doc). They
/// are told by the predicted size, which [`apart_deg`] works out with no
/// images, and by a misregistration of 8 px or more.
#[test]
fn seam_signature_attributes_planted_errors() {
    let _t = timed("seam_signature_attributes_planted_errors");
    let first = &[A1, B1, C1, D1][..];
    let depth = &[A0, D1][..];
    // Nothing planted, and what the seam can't see.
    let nothing = [
        ("the truth's model", Plant::Truth),
        ("the truth's model, the scene at 1.5 m", Plant::Near),
        ("the V6 camera read as rendered", Plant::V6),
        ("the Kannala–Brandt rig", Plant::Kb),
        ("both lenses turned 0.5° about +Y", Plant::CommonMode),
    ];
    // The controls of 4.2 and the rows of 3.4 that the search holds.
    let planted = [
        ("lens 1 yawed 0.5°", Plant::Yaw, &[A1][..]),
        ("lens 1 pitched 0.5°", Plant::Pitch, &[B1]),
        ("lens 1 rolled 0.5°", Plant::Roll, &[C0]),
        ("lens 1 turned 0.13° about +Y", Plant::SmallTurn, &[A1]),
        ("f × 1.012, C read as A", Plant::MapA, &[A0]),
        ("lens 0 alone under A", Plant::OneLensA, &[A0]),
        ("cx + 2 px on lens 1", Plant::Centre, first),
        ("the tracks swapped", Plant::Swapped, first),
        // Parallax moves the seam along itself too, wherever the lenses'
        // centres lie apart in the seam's plane: d1 comes with a0.
        ("q = 0 against R = 1.5 m", Plant::NearAtInfinity, depth),
        ("tz's sign flipped at 1.5 m", Plant::TzFlipped, depth),
        (
            "lens 1 a frame late in a pan about +Y",
            Plant::LatePan,
            &[A1],
        ),
        (
            "lens 1 a frame late in a turn about the lens axis",
            Plant::LateRoll,
            &[C0],
        ),
        ("lens 0's yaw with the wrong sign", Plant::EulerYaw0, &[A1]),
        (
            "both pitches with the wrong sign",
            Plant::EulerPitches,
            &[B1],
        ),
        ("both yaws with the wrong sign", Plant::EulerYaws, &[A1]),
        ("the V6 terms read as RadialOnly", Plant::V6Radial, first),
        (
            "lens 0's V6 terms read as RadialOnly",
            Plant::V6Radial0,
            first,
        ),
        (
            "lens 1's V6 terms read as RadialOnly",
            Plant::V6Radial1,
            first,
        ),
        (
            "the V6 terms read as {D6D5 with extension, D9 Interleaved}",
            Plant::V6Interleaved,
            first,
        ),
        ("the V6 terms read as {D5D6, D7 ByAxis}", Plant::V6D7, first),
        (
            "Pat's V3 lens 0 under V6's model",
            Plant::PatsModel0,
            &[A0, A1, B1, C1, D1],
        ),
        (
            "Pat's V3 lens 1 under V6's model",
            Plant::PatsModel1,
            &[A0, A1, B1, C1, D1],
        ),
        (
            "Pat's V3 lens 0 at V6's angles",
            Plant::PatsAngles0,
            &[A1, B1],
        ),
        (
            "Pat's V3 lens 1 at V6's angles",
            Plant::PatsAngles1,
            &[A1, B1],
        ),
    ];
    // Past the search.
    let gross = [
        ("equidistant applied to Mei data", Plant::Equidistant),
        ("the turn left out", Plant::NoTurn),
        (
            "the Kannala–Brandt lenses read as equisolid",
            Plant::KbEquisolid,
        ),
    ];
    let all = (nothing.iter().chain(&gross).map(|row| row.1))
        .chain(planted.iter().map(|row| row.1))
        .chain([Plant::Gains]);
    warm(&all.collect::<Vec<Plant>>());

    for (name, plant) in nothing {
        let seam = plant.seam();
        assert_eq!(
            (seam.held, seam.unmeasured()),
            (seam.predicted.len(), 0),
            "{name}"
        );
        assert!(seam.off_px <= PREDICTED_PX, "{name}: {} px", seam.off_px);
        let found = attribute(&seam, Lands::Nowhere).unwrap_or_else(|e| panic!("{name}: {}", e.1));
        eprintln!(
            "8.3 | {name}: {:.4} px off the prediction; {found}",
            seam.off_px
        );
        hold_seam(name, &seam);
    }

    for (name, plant, named) in planted {
        let seam = plant.seam();
        // Every bin is predicted and measured; of the tracks swapped,
        // every bin that the search holds.
        let bins = if plant == Plant::Swapped {
            SWAPPED_BINS
        } else {
            seam.predicted.len()
        };
        assert_eq!((seam.held, seam.unmeasured()), (bins, 0), "{name}");
        assert!(seam.off_px <= PREDICTED_PX, "{name}: {} px", seam.off_px);
        let found =
            attribute(&seam, Lands::On(named)).unwrap_or_else(|e| panic!("{name}: {}", e.1));
        // Each is a control of 4.2's misregistration.
        let px = must_fail(
            &format!("{name}, the misregistration"),
            at_most(seam.misregistration_px, MISREGISTRATION_PX),
        );
        eprintln!(
            "8.3 | {name}: {} bins, {:.4} px off the prediction; {found}; misregistered {px:.3} px",
            seam.held, seam.off_px
        );
    }

    // Controls of the bins a row is held over: the tracks swapped, held
    // to be predicted in every bin; and track 1 showing another scene,
    // which every bin is predicted for and few are measured at.
    let swapped = Plant::Swapped.seam();
    must_fail(
        "the tracks swapped, a bin predicted for each",
        swapped.held == swapped.predicted.len(),
    );
    let other = Plant::OtherScene.seam();
    assert_eq!(other.held, other.predicted.len());
    must_fail(
        "track 1 showing another scene, each predicted bin measured",
        other.unmeasured() == 0,
    );

    // The tracks swapped never land on c0, nor collapse the NCC.
    assert!(swapped.got.c0.abs() < TERM_DEG, "{}", line(&swapped.got));
    assert!(swapped.got.median_ncc >= NCC, "{}", swapped.got.median_ncc);

    // Gains off land on the log gains, and nowhere in the geometry.
    let gains = Plant::Gains.seam();
    assert_eq!((gains.held, gains.unmeasured()), (gains.predicted.len(), 0));
    assert!(gains.off_px <= PREDICTED_PX, "{} px", gains.off_px);
    let found = attribute(&gains, Lands::Nowhere).unwrap_or_else(|e| panic!("gains off: {}", e.1));
    let log_gains = gained().lenses[1].gain.map(f64::ln);
    for (c, (got, planted)) in gains.own.log_gain.iter().zip(&log_gains).enumerate() {
        let off = (got - planted).abs();
        assert!(off <= LOG_GAIN, "channel {c}: {off}");
    }
    eprintln!(
        "8.3 | gains off: log gains {:.4?} for {log_gains:.4?} planted; {:.4} px off the \
         prediction; {found}",
        gains.own.log_gain, gains.off_px
    );

    // Control of step 2: a field held to the prediction for another
    // error.
    let (yaw, pitch) = (Plant::Yaw.seam(), Plant::Pitch.seam());
    let (sx, sy) = scales();
    let apart = yaw.measured.iter().zip(&pitch.predicted).map(|(m, p)| {
        ((m.across_deg - p.across_deg) * sy).powi(2) + ((m.along_deg - p.along_deg) * sx).powi(2)
    });
    let apart = (apart.sum::<f64>() / yaw.measured.len() as f64).sqrt();
    let px = must_fail(
        "the yaw's field against the prediction for a pitch",
        at_most(apart, PREDICTED_PX),
    );
    eprintln!("8.3 | control: the yaw's field is {px:.3} px off the pitch's prediction");

    // Controls of step 3, one for each of its rules, each held to break
    // that rule and no earlier one. The first two read a field against
    // the test's own list; the last three read the measured terms.
    let centre = Plant::Centre.seam();
    let radial_1 = Plant::V6Radial1.seam();
    let breaks = |name: &str, found: Attributed, rule: Misfit| {
        let (broken, said) = must_fail(name, found);
        assert_eq!(broken, rule, "{name}: {said}");
        eprintln!("8.3 | control, {name}: {said}");
    };
    breaks(
        "the yaw's field read as no shift",
        attribute(&yaw, Lands::Nowhere),
        Misfit::Shifted,
    );
    breaks(
        "the yaw's field read as a roll",
        attribute(&yaw, Lands::On(&[C0])),
        Misfit::Unnamed,
    );
    breaks(
        "the yaw's field for the pitch's prediction",
        attribute_to(&pitch.want, &yaw.got, Lands::On(&[A1, B1])),
        Misfit::Elsewhere,
    );
    // Two named terms predicted one size may come either way round:
    // lens 0's V6 terms read as RadialOnly, with b1 and c1, predicted
    // 0.0025° apart, the other way round in the images. Predicted two
    // bounds apart, they may not.
    let radial_0 = Plant::V6Radial0.seam();
    let (main, next) = (radial_0.want.c1.abs(), radial_0.want.b1.abs());
    assert!(main > next && main - next < 0.1 * TERM_DEG, "{main} {next}");
    assert!(radial_0.got.c1.abs() > radial_0.got.b1.abs());
    let b1_first = Signature {
        c1: 0.99 * radial_0.got.c1,
        ..radial_0.got
    };
    assert!(b1_first.b1.abs() > b1_first.c1.abs());
    let found = attribute_to(&radial_0.want, &b1_first, Lands::On(first))
        .unwrap_or_else(|e| panic!("b1 over c1 in the images: {}", e.1));
    eprintln!("8.3 | lens 0's V6 terms read as RadialOnly, b1 over c1 in the images: {found}");
    let b1_predicted_less = Signature {
        b1: radial_0.want.b1 - 2.0 * TERM_DEG,
        ..radial_0.want
    };
    breaks(
        "a named term two bounds under the prediction's largest, the largest in the images",
        attribute_to(&b1_predicted_less, &b1_first, Lands::On(first)),
        Misfit::Elsewhere,
    );
    breaks(
        "cx + 2 px on lens 1 read as a tilt alone",
        attribute(&centre, Lands::On(&[A1])),
        Misfit::Crosstalk,
    );
    // Both land on a1, and their coupled terms tell them apart.
    breaks(
        "cx + 2 px on lens 1 for the prediction of lens 1's V6 terms read as RadialOnly",
        attribute_to(&radial_1.want, &centre.got, Lands::On(first)),
        Misfit::Ratio,
    );
    // Parallax on this rig comes with d1 at its predicted ratio to a0:
    // a field of a0 alone isn't the stitch distance's, nor is one whose
    // d1 has the other sign.
    let at_infinity = Plant::NearAtInfinity.seam();
    for (name, d1) in [
        ("no d1", 0.0),
        ("d1 of the other sign", -at_infinity.got.d1),
    ] {
        let across_alone = Signature {
            d1,
            ..at_infinity.got
        };
        breaks(
            &format!("q = 0 against R = 1.5 m with {name}"),
            attribute_to(&at_infinity.want, &across_alone, Lands::On(depth)),
            Misfit::Ratio,
        );
    }

    // What tells the fields past the search, held where its answer is
    // known: nothing apart under the truth's own model, and 0.5° all
    // round the seam with lens 1 rolled 0.5°. Each is the other's
    // control.
    let apart_of = |plant: Plant| {
        let setup = plant.setup();
        apart_deg(&setup.truth, &setup.rig)
    };
    let (still, rolled) = (apart_of(Plant::Truth), apart_of(Plant::Roll));
    let all_within = |apart: [f64; 3], of: f64, by: f64| {
        at_most(apart.iter().map(|a| (a - of).abs()).fold(0.0, worse), by)
    };
    assert!(
        all_within(still, 0.0, APART_STILL_DEG).passes() == Some(true),
        "{still:?}"
    );
    assert!(
        all_within(rolled, 0.5, APART_DEG).passes() == Some(true),
        "{rolled:?}"
    );
    must_fail(
        "lens 1 rolled 0.5°, the seam held to lie nowhere apart",
        all_within(rolled, 0.0, APART_STILL_DEG),
    );
    must_fail(
        "the truth's model, the seam held to lie 0.5° apart",
        all_within(still, 0.5, APART_DEG),
    );
    eprintln!(
        "8.3 | apart_deg: {:.2e}° at most under the truth's model; {:.5}° to {:.5}° with lens 1 \
         rolled 0.5°",
        still[2], rolled[1], rolled[2]
    );

    for (name, plant) in gross {
        let seam = plant.seam();
        let [mean, least, most] = apart_of(plant);
        assert_eq!(seam.held, 0, "{name}");
        assert!(mean > SPEC.search_deg, "{name}: {mean}°");
        assert!(seam.own.valid_bins > 0, "{name}");
        let px = must_fail(
            &format!("{name}, the misregistration"),
            at_most(seam.misregistration_px, MISREGISTRATION_PX),
        );
        assert!(px >= 8.0, "{name}: {px} px");
        // No bin is predicted, so step 3 has no term to read.
        let (broken, said) = must_fail(
            &format!("{name}, attributed by its terms"),
            attribute(&seam, Lands::Nowhere),
        );
        assert_eq!(broken, Misfit::Unmeasured, "{name}: {said}");
        eprintln!(
            "8.3 | {name}: past the search. Predicted: the two lenses show the seam {mean:.2}° \
             apart (mean; {least:.2}° to {most:.2}°), no bin within the search of {}°. Measured: \
             {} of {} bins, misregistered {px:.2} px, median NCC {:.4}",
            SPEC.search_deg,
            seam.own.valid_bins,
            seam.measured.len(),
            seam.own.median_ncc
        );
    }
}

/// 3.4's budget, its pixels per degree across and along the seam and the
/// size of each planted difference, holds on the images: each row's
/// terms, measured from the strips, against the table's figure at 768²,
/// within 15 % or 0.2 px, the seam's own tests' tolerance. Where those
/// tests found the table's figure wrong (`predicted_fields_match_the_error_budget`),
/// the images are held to the figure they worked again.
///
/// "V3 against V6" is measured on Pat's V3 pictures with a lens's model,
/// or its angles, from Pat's V6 blocks: the difference the seam's own
/// tests predict, seen from the other side. Of the V6 readings, the
/// images hold the one the pair is rendered under against `RadialOnly`,
/// a lens at a time; the other two readings' rows would each take a
/// rendered pair, and stay with the predictions. The row at 10 m has no
/// figure at 768².
#[test]
fn error_budget_matches_the_images() {
    let _t = timed("error_budget_matches_the_images");
    let x5 = x5();
    warm(&[
        Plant::PatsModel0,
        Plant::V6Radial0,
        Plant::LatePan,
        Plant::LateRoll,
        Plant::NearAtInfinity,
        Plant::TzFlipped,
        Plant::PatsModel1,
        Plant::PatsAngles0,
        Plant::PatsAngles1,
        Plant::V6Radial1,
        Plant::MapA,
        Plant::OneLensA,
        Plant::Roll,
        Plant::Yaw,
        Plant::Pitch,
        Plant::Centre,
        Plant::EulerYaw0,
        Plant::EulerPitches,
        Plant::EulerYaws,
        Plant::Swapped,
    ]);
    let mut budget = Budget::of("measured");
    let got = |plant: Plant| plant.seam().got;
    // What a plant moves a bin by, over the bins the prediction holds.
    let moved = |plant: Plant| {
        let seam = plant.seam();
        let held: Vec<BinShift> = seam
            .measured
            .iter()
            .zip(&seam.predicted)
            .map(|(m, p)| BinShift {
                valid: m.valid && p.valid,
                ..*m
            })
            .collect();
        sizes(&held)
    };

    // Q4.
    budget.hold(
        "Q4, C against A, a0",
        got(Plant::MapA).a0.abs() * ACROSS_PX,
        8.3,
    );
    budget.hold(
        "Q4, one lens under A, a0",
        got(Plant::OneLensA).a0.abs() * ACROSS_PX,
        1.09 * ACROSS_PX,
    );

    // A relative roll, a relative tilt, and lens 1's centre 2 px off.
    budget.hold(
        "relative roll 0.5°, c0",
        got(Plant::Roll).c0.abs() * ALONG_PX,
        3.0,
    );
    budget.hold(
        "relative tilt 0.5°, a yaw, a1",
        got(Plant::Yaw).a1.abs() * ACROSS_PX,
        1.9,
    );
    budget.hold(
        "relative tilt 0.5°, a pitch, b1",
        got(Plant::Pitch).b1.abs() * ACROSS_PX,
        1.9,
    );
    let (across, along) = harmonics(&got(Plant::Centre));
    budget.hold(
        "cx + 2 px on lens 1, a1/b1",
        across * ACROSS_PX,
        0.52 * ACROSS_PX,
    );
    budget.hold(
        "cx + 2 px on lens 1, c1/d1",
        along * ALONG_PX,
        0.33 * ALONG_PX,
    );
    let size = moved(Plant::Centre);
    budget.hold(
        "cx + 2 px on lens 1, the least a bin moves",
        size.least,
        2.0,
    );
    budget.hold("cx + 2 px on lens 1, the most a bin moves", size.most, 2.0);

    // The stitch distance, and the sign of tz.
    budget.hold(
        "∞ against 1.5 m, a0",
        got(Plant::NearAtInfinity).a0.abs() * ACROSS_PX,
        4.6,
    );
    budget.hold(
        "tz flipped at 1.5 m, a0",
        got(Plant::TzFlipped).a0.abs() * ACROSS_PX,
        9.2,
    );

    // Lens 1 a frame late while the rig turns 10° a second.
    budget.hold(
        "lens 1 a frame late in a pan about +Y, a1",
        got(Plant::LatePan).a1.abs() * ACROSS_PX,
        1.3,
    );
    budget.hold(
        "lens 1 a frame late in a turn about the lens axis, c0",
        got(Plant::LateRoll).c0.abs() * ALONG_PX,
        2.0,
    );

    // The Euler class.
    let ypr = [x5.cal.blocks[0].ypr_deg, x5.cal.blocks[1].ypr_deg];
    budget.hold(
        "the Euler class, lens 0's yaw of 0.3° with the wrong sign, a1",
        got(Plant::EulerYaw0).a1.abs() * ACROSS_PX,
        2.3,
    );
    budget.hold(
        "the Euler class, both pitches with the wrong sign, b1",
        got(Plant::EulerPitches).b1.abs() * ACROSS_PX,
        1.2,
    );
    budget.hold_corrected(
        "the Euler class, both yaws with the wrong sign, a1",
        got(Plant::EulerYaws).a1.abs() * ACROSS_PX,
        2.0 * (ypr[0][0] - ypr[1][0]).abs() * ACROSS_PX,
        2.3,
        "one lens's",
    );

    // The V6 reading the pair is rendered under against RadialOnly, a
    // lens at a time: 8.2's figures at 3840² are five times 768²'s.
    for (b, plant, at_3840) in [(0, Plant::V6Radial0, 33.0), (1, Plant::V6Radial1, 20.5)] {
        let row =
            format!("V6 reading {{D5D6 with extension, D9 ByAxis}} against RadialOnly, lens {b}");
        budget.hold(
            &format!("{row}, the most a bin moves"),
            moved(plant).most,
            at_3840 / 5.0,
        );
    }

    // The binding reversed in Tier 1's rig, over the bins the search
    // holds: the angles' tilt and the centres' difference across, and the
    // centres' other part along.
    let centres = centres_apart(&x5.cal);
    let (across, along) = harmonics(&got(Plant::Swapped));
    let tilt = 2.0 * (ypr[0][0] - ypr[1][0]);
    budget.hold_corrected(
        "the binding reversed, Tier 1's rig, a1/b1",
        across * ACROSS_PX,
        (tilt + 2.0 * CENTRE_ACROSS_DEG * centres[0]).abs() * ACROSS_PX,
        9.0,
        "without the centres' part across",
    );
    budget.hold_worked(
        "the binding reversed, Tier 1's rig, c1/d1",
        along * ALONG_PX,
        2.0 * CENTRE_ALONG_DEG * centres[1].abs() * ALONG_PX,
    );

    // V3 against V6 on Pat's blocks, a lens at a time: the intrinsics,
    // then the angles.
    let pats = pats_v3();
    for (b, model, angles, moves, tilt) in [
        (0, Plant::PatsModel0, Plant::PatsAngles0, [2.2, 3.4], 1.8),
        (1, Plant::PatsModel1, Plant::PatsAngles1, [1.6, 2.6], 1.1),
    ] {
        let row = format!("V3 against V6, lens {b}");
        let size = moved(model);
        budget.hold(
            &format!("{row}, the least the intrinsics move a bin"),
            size.least,
            moves[0],
        );
        budget.hold(
            &format!("{row}, the most the intrinsics move a bin"),
            size.most,
            moves[1],
        );
        // a0: the seam's circle is this much longer in V6's picture, so
        // V3's picture under V6's model comes out that much shorter. Both
        // lenses' are under the budget's 0.2 px, which a zero would fit
        // as well, so each is held with its sign to 0.05 px.
        let longer = seam_radius(&pats.as_v6.lenses[b]) - seam_radius(&pats.as_v3.lenses[b]);
        let a0 = got(model).a0 * ACROSS_PX;
        budget.hold_small(&format!("{row}, a0"), a0, -longer);
        // Controls: no a0 at all, and one of the other sign.
        let none = must_fail(&format!("{row}, no a0"), small(0.0, -longer));
        let other = must_fail(
            &format!("{row}, a0 with the other sign"),
            small(-a0, -longer),
        );
        assert!(none > 2.0 * SMALL_PX && other > none, "{none} {other}");
        if b == 0 {
            budget.hold_corrected(
                &format!("{row}, a0"),
                a0,
                -longer,
                -0.15 * ACROSS_PX,
                "fx alone",
            );
        }
        budget.hold(
            &format!("{row}, the angles' tilt, a1/b1"),
            harmonics(&got(angles)).0 * ACROSS_PX,
            tilt,
        );
    }

    // Control: the budget's check fails on a figure that is wrong, the
    // yaw's term held to the roll's row.
    let mut control = Budget::of("measured");
    control.hold(
        "control, the yaw's a1 against the roll's 3.0 px",
        got(Plant::Yaw).a1.abs() * ACROSS_PX,
        3.0,
    );
    must_fail(
        "the yaw's a1 held to the roll's row",
        control.missed.is_empty(),
    );

    assert!(
        budget.missed.is_empty(),
        "3.4's rows that the images miss: {:#?}",
        budget.missed
    );
}
