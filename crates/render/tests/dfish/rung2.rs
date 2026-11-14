//! Rung 2 (M3 plan 4.2): rung 1's planes stitched by the shader on
//! lavapipe, held to rung 1's pictures (`reference::render_rig_of`,
//! through `tests/common/rung1.rs`) by D32's reference rule, over the
//! shots rung 1 holds: the 512 × 256 equirect and the four band stripes,
//! the stitch and each block alone. A module of the goldens binary, whose
//! pairs it shares; it stores no PNG, and blesses its floors into
//! `goldens/rung2.json` by a ledger of its own tests.
//!
//! Each picture ([`Picture`]) is drawn by `StillRenderer` from the pair's
//! bytes, as `render_blurred` draws a pair, and rendered by rung 1 from
//! the same frames with the same rig, gains, shot and mode. The rule
//! ([`Row::rule`]) is the render goldens' CPU rule: the PSNR over the
//! whole picture no lower than the floor blessed for it (what it measured
//! then, less 3 dB, and never below 36 dB), and no channel more than 0.1
//! off. Modes 6 and 7 are read back in linear light, where the seam
//! difference passes 1, and rung 1 rounds them to the nearest f16 as the
//! target does; the seam difference is held in the lenses' light, over
//! the 8 it is drawn at ([`Picture::held`]). Where a picture draws a mask
//! edge to edge (a block alone, or the seam difference), a pixel whose
//! mask falls within f32's rounding of its bound may fall either way, in
//! f32 on the GPU and f64 in rung 1: such a pixel is held to nothing, and
//! counted ([`Picture::untie`]).
//!
//! The pairs are rung 1's four, each made once for the binary: the
//! X5-like camera's `SeamLines` and `Grid` infinitely far, its
//! `SeamLines` 1.5 m away, and the Kannala–Brandt camera's `SeamLines`,
//! each 768² a lens. The dfish rows draw the first two too.
//!
//! `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test
//! goldens -- rung2_` blesses the floors, by the variable `goldens.rs`
//! blesses its own by: `goldens/rung2.json` is written once every one of
//! [`TESTS`] has passed and every control has failed, and
//! `SPHERECUT_UPDATE_GOLDENS=lower` lets a floor drop by more than 0.5 dB.
//!
//! Budget (19.6): 1.5 s of the goldens binary's growth, the pairs
//! included; each test prints its time and rung 2's so far.
//!
//! [`rung2_on_radv`] is rung 7's rung 2: the same pictures on each
//! hardware GPU, against lavapipe's by D32's other-driver rule and
//! against rung 1 by the reference rule, at the floors blessed here.

use std::collections::BTreeMap;
use std::f64::consts::FRAC_PI_2;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use glam::DQuat;
use serde_json::{Map, Value, json};
use spherecut_render::reference::{ImageF64, RigDebug, lens_footprint};
use spherecut_render::still::srgb_eotf;
use spherecut_render::{
    AdapterChoice, Depth, Gpu, Image, LensFrame, Pixels, PlaneSource, SourceFrames, StillOptions,
    StillRenderer, ViewParams,
};
use spherecut_sphere::perspective::Projection;
use spherecut_sphere::pixel;
use spherecut_sphere::rig::Rig;
use spherecut_testkit::control::{above, at_least, at_most, must_fail};
use spherecut_testkit::{bench, bless, tier2};

use super::common::attribution::equisolid_for;
use super::common::rung1::{
    Pair, Scene, Shot, UNIT, VIEW_KEYS, both, equirect, grid, kb, lines, near_lines, stitch, view,
    x5,
};
use super::common::{gpu, hardware_gpu, hardware_gpus};
use super::{
    Driver, OTHER_PAST_LSB, drift, head_commit, judge, lens, max_diff, num, psnr_of, today, unit,
};
// D32's reference rule and how its floors are blessed: `goldens.rs`'s own
// constants, with its blessing's variable.
use super::{CPU_FLOOR_DB, CPU_MARGIN_DB, CPU_MAX_CHANNEL, FLOOR_DROP_DB, LOWER, UPDATE};

// The clock, and what a run blesses.

/// Rung 2's share of the goldens binary's budget, in seconds (the
/// module's doc).
const BUDGET_S: f64 = 1.5;

/// Rung 2's tests on lavapipe. A blessing is written once each has ended
/// well.
const TESTS: [&str; 5] = [
    "rung2_x5_like_matches_rung1",
    "rung2_near_matches_rung1",
    "rung2_kb_rig_matches_rung1",
    "rung2_modes_6_and_7_match_rung1",
    "rung2_unlike_gains_under_a_reversed_binding",
];

/// When rung 2's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, as `tests/instrument.rs` keeps one: it prints as the
/// test ends how long the test took and how long rung 2 has run, and
/// tells the ledger how the test ended. The tests run side by side, so
/// the last line printed is rung 2's time.
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
        let so_far = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; rung 2 {so_far:.3} s so far, of its {BUDGET_S} s",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
        ledger().end(self.name, !std::thread::panicking());
    }
}

/// Where the floors are kept, beside the goldens' manifests.
const MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/rung2.json");
const REGENERATE: &str =
    "SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test goldens -- rung2_";

/// The manifest as read, or why there is none.
fn manifest() -> &'static Result<Value, String> {
    static READ: OnceLock<Result<Value, String>> = OnceLock::new();
    READ.get_or_init(|| {
        std::fs::read_to_string(MANIFEST)
            .map_err(|e| format!("no manifest at {MANIFEST} ({e})"))
            .and_then(|t| {
                serde_json::from_str(&t).map_err(|e| format!("{MANIFEST} doesn't parse: {e}"))
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

/// The floor a blessing gives a picture that measures `psnr`: 3 dB under
/// it, to 0.01 dB, and never below 36 dB, as `Run::calibrated` gives a
/// CPU check's.
fn calibrated(psnr: f64) -> f64 {
    (((psnr - CPU_MARGIN_DB) * 100.0).floor() / 100.0).max(CPU_FLOOR_DB)
}

/// The reference rule on a picture's PSNR and largest difference, held
/// to `floor`. A NaN compares with nothing, and fails.
fn reference_rule(psnr: f64, max: f64, floor: f64) -> Result<(), String> {
    if at_least(psnr, floor).passes() != Some(true) {
        Err(format!("PSNR {psnr:.4} dB, below its floor {floor}"))
    } else if at_most(max, CPU_MAX_CHANNEL).passes() != Some(true) {
        Err(format!(
            "a channel {max:.4} off, more than {CPU_MAX_CHANNEL}"
        ))
    } else {
        Ok(())
    }
}

/// A control's name as a key of the manifest: its letters and digits in
/// lower case, joined by `_`.
fn slug(name: &str) -> String {
    let words = name.split(|c: char| !c.is_ascii_alphanumeric());
    let words: Vec<String> = words
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    words.join("_")
}

/// One row of the manifest: a pair's pictures, their floors, and what
/// they and their controls measured.
struct Row {
    name: &'static str,
    what: &'static str,
    /// Whether the floors are calibrated here (a blessing) or read from
    /// the manifest.
    blessing: bool,
    thresholds: Map<String, Value>,
    measured: Map<String, Value>,
    failures: Vec<String>,
    /// Pixels held to nothing at a tie of a mask, over the row.
    ties: usize,
}

impl Row {
    /// A row of a test on lavapipe, which a blessing calibrates.
    fn new(name: &'static str, what: &'static str) -> Row {
        Row {
            blessing: bless::on(UPDATE),
            ..Row::reading(name, what)
        }
    }

    /// A row held to the blessed floors whatever the run: rung 7's.
    fn reading(name: &'static str, what: &'static str) -> Row {
        Row {
            name,
            what,
            blessing: false,
            thresholds: Map::new(),
            measured: Map::new(),
            failures: Vec::new(),
            ties: 0,
        }
    }

    /// Notes the pixels of picture `key` held to nothing at a tie of a
    /// mask ([`Picture::untie`]).
    fn tied(&mut self, key: &str, ties: &[(u32, u32)]) {
        self.ties += ties.len();
        if !ties.is_empty() {
            eprintln!("{}: {key} ties at its mask at {ties:?}", self.name);
            self.measure(&format!("{key}_mask_ties"), ties.len() as f64);
        }
    }

    fn measure(&mut self, key: &str, v: f64) {
        eprintln!("{}: {key} = {v:.4}", self.name);
        self.measured.insert(key.into(), num(v));
    }

    fn fail(&mut self, msg: String) {
        eprintln!("FAIL {}: {msg}", self.name);
        self.failures.push(format!("{}: {msg}", self.name));
    }

    /// The floor picture `key` is held to: the one this run put in the
    /// row's thresholds, blessed or calibrated.
    fn floor(&self, key: &str) -> Option<f64> {
        let tkey = format!("{key}_psnr_db_min");
        self.thresholds.get(&tkey).and_then(Value::as_f64)
    }

    /// Picture `key`, drawn as `got` and rendered by rung 1 as `want`,
    /// held to the reference rule. A blessing that would lower a blessed
    /// floor by more than 0.5 dB fails, unless it was told it may.
    fn rule(&mut self, key: &str, size: (u32, u32), got: &[f64], want: &[f64]) {
        let (psnr, max) = (psnr_of(size, got, want), max_diff(got, want));
        self.measure(&format!("{key}_psnr_db"), psnr);
        self.measure(&format!("{key}_max_channel"), max);
        let tkey = format!("{key}_psnr_db_min");
        let floor = if self.blessing {
            let new = calibrated(psnr);
            let old = blessed(self.name, &tkey).ok();
            let was = old.map_or_else(|| "new".into(), |o| o.to_string());
            ledger()
                .floors
                .push(format!("{} {tkey}: {was} → {new}", self.name));
            if let Some(old) = old
                && new < old - FLOOR_DROP_DB
                && !bless::lower(UPDATE)
            {
                self.fail(format!(
                    "blessing would lower {tkey} from {old} to {new} (measured {psnr:.2}), by \
                     more than {FLOOR_DROP_DB} dB: a stitch that got worse, or another driver \
                     build; to accept it, bless with {UPDATE}={LOWER}"
                ));
            }
            new
        } else {
            match blessed(self.name, &tkey) {
                Ok(floor) => floor,
                Err(e) => {
                    self.fail(e);
                    return;
                }
            }
        };
        self.thresholds.insert(tkey, json!(floor));
        self.thresholds
            .insert(format!("{key}_max_channel_max"), num(CPU_MAX_CHANNEL));
        if let Err(e) = reference_rule(psnr, max, floor) {
            self.fail(format!("{key}: {e}"));
        }
    }

    /// Control `name`'s picture against rung 1's true picture `key`: what
    /// it measured, and what the reference rule at `key`'s floor came to,
    /// which the control must fail. Gives the PSNR, the largest
    /// difference and the rule's outcome.
    fn control(
        &mut self,
        name: &str,
        key: &str,
        size: (u32, u32),
        got: &[f64],
        want: &[f64],
    ) -> (f64, f64, Result<(), String>) {
        let (psnr, max) = (psnr_of(size, got, want), max_diff(got, want));
        let slug = slug(name);
        self.measure(&format!("control_{slug}_psnr_db"), psnr);
        self.measure(&format!("control_{slug}_max_channel"), max);
        let floor = self
            .floor(key)
            .unwrap_or_else(|| panic!("{}: {key} has no floor this run", self.name));
        (psnr, max, reference_rule(psnr, max, floor))
    }

    /// Fails with every failure the row gathered, or hands the row to the
    /// ledger, for a blessing to write.
    fn finish(mut self) {
        self.measure("mask_ties", self.ties as f64);
        assert!(
            self.failures.is_empty(),
            "{} checks failed:\n  {}",
            self.failures.len(),
            self.failures.join("\n  ")
        );
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
        if !bless::on(UPDATE) {
            return;
        }
        let said = match Blessing::after(&self.ended) {
            Blessing::Refused(failed) => format!("rung 2 not blessed: {failed:?} failed\n"),
            Blessing::Waits(waits) => {
                format!("{name} ended well; rung 2's blessing waits on {waits:?}\n")
            }
            Blessing::Writes => self.write(),
        };
        // Straight to the terminal: a passing test's eprintln! is
        // captured, and whoever blesses should see the floors move.
        use std::io::Write;
        let _ = std::io::stderr().lock().write_all(said.as_bytes());
    }

    fn write(&self) -> String {
        let driver = Driver::of(gpu());
        let manifest = json!({
            "schema": "spherecut.rung2/1",
            "about": "Rung 2 of the stitching round trip (M3 plan 4.2, DECISIONS D32): rung 1's \
                      lens pairs stitched by the shader on lavapipe against rung 1's pictures \
                      by the reference rule, with each picture's blessed floor and what it and \
                      the controls measured when blessed.",
            "regenerate": REGENERATE,
            "adapter": driver.adapter,
            "driver": driver.driver,
            "driver_info": driver.driver_info,
            "llvm": driver.llvm(),
            "date": today(),
            "commit": head_commit(),
            "rows": self.rows,
        });
        let text = serde_json::to_string_pretty(&manifest).unwrap() + "\n";
        std::fs::write(MANIFEST, text).unwrap();
        format!(
            "blessed {} rows into {MANIFEST}, with these floors (old → new):\n  {}\n",
            self.rows.len(),
            self.floors.join("\n  ")
        )
    }
}

// The pictures.

/// One picture rung 2 holds: a pair (by track) stitched by `rig` with
/// `gains` (by block) through `shot` in `debug`'s mode, read back at
/// `depth`, under `key` in its row.
#[derive(Debug, Clone)]
struct Picture {
    row: &'static str,
    key: String,
    frames: Pair,
    rig: Rig,
    gains: [[f64; 3]; 2],
    shot: Shot,
    debug: RigDebug,
    depth: Depth,
}

impl Picture {
    /// The picture drawn on `gpu`, with `rig`, `gains` and `depth` in its
    /// own's place: a control's.
    fn draw_by(
        &self,
        renderer: &mut StillRenderer,
        gpu: &Gpu,
        rig: &Rig,
        gains: [[f64; 3]; 2],
        depth: Depth,
    ) -> Image {
        let lenses = self.frames.map(|frame| LensFrame {
            frame,
            from: PlaneSource::Bytes,
        });
        let opts = StillOptions {
            debug: self.debug.mode(),
            depth,
            ..StillOptions::default()
        };
        let source = SourceFrames::Pair { lenses, rig, gains };
        let views = std::slice::from_ref(&self.shot.0);
        renderer
            .render_blurred(gpu, source, views, self.shot.1, &opts)
            .unwrap_or_else(|e| panic!("{} {}: {e} ({})", self.row, self.key, e.code()))
    }

    fn draw(&self, renderer: &mut StillRenderer, gpu: &Gpu) -> Image {
        self.draw_by(renderer, gpu, &self.rig, self.gains, self.depth)
    }

    /// Rung 1's picture: display values, or for modes 6 and 7 linear
    /// light as the nearest f16, as `unit` reads the drawn one back.
    fn rung1(&self) -> ImageF64 {
        stitch(self.frames, &self.rig, self.gains, &self.shot, self.debug)
    }

    /// What one unit of the picture is worth in the lenses' light: the
    /// seam difference (mode 7) draws |g0·lin0 − g1·lin1| eight times
    /// over, every other mode as it is.
    fn scale(&self) -> f64 {
        if self.debug == RigDebug::SeamDiff {
            1.0 / SEAM_DIFF_GAIN
        } else {
            1.0
        }
    }

    /// The drawn picture as rung 2 holds it, each channel in `[0, 1]`
    /// and the seam difference back in the lenses' light
    /// ([`Self::scale`]): a seam difference's rounding is the lenses'
    /// own, eight times over, as rung 1's is.
    fn held(&self, img: &Image) -> Vec<f64> {
        let s = self.scale();
        unit(img).into_iter().map(|v| v * s).collect()
    }

    /// Rung 1's picture as rung 2 holds it ([`Self::held`]).
    fn held_rung1(&self, img: &ImageF64) -> Vec<f64> {
        let s = self.scale();
        img.data.iter().map(|v| v * s).collect()
    }

    /// A drawn picture as rung 2 holds it, as an image: the seam
    /// difference's linear values over eight, every other mode as drawn.
    fn held_image(&self, img: &Image) -> Image {
        match &img.pixels {
            Pixels::LinearF32(p) if self.debug == RigDebug::SeamDiff => Image {
                pixels: Pixels::LinearF32(p.iter().map(|v| v / SEAM_DIFF_GAIN as f32).collect()),
                ..*img
            },
            _ => img.clone(),
        }
    }

    /// The blocks whose masks the picture draws edge to edge, with no
    /// feather to soften them: the block alone (modes 4 and 5), both for
    /// the seam difference, which is black wherever one doesn't cover.
    fn masks(&self) -> &'static [usize] {
        match self.debug {
            RigDebug::Lens0 => &[0],
            RigDebug::Lens1 => &[1],
            RigDebug::SeamDiff => &[0, 1],
            _ => &[],
        }
    }

    /// Whether block `b`'s mask decides pixel `(i, j)` within
    /// [`TIE_TEXELS`] of its bound: `Rig::covers` at the footprint's LOD
    /// (`reference::lens_footprint`), the least of its three margins in
    /// texels, θ's to θ_max less margin(λ) and the texel's to each side
    /// of its slot less margin(λ), within that of 0. A pixel with no
    /// footprint or no image covers nothing, by no margin, and never
    /// ties.
    fn ties_at(&self, b: usize, i: u32, j: u32) -> bool {
        let (view, size) = &self.shot;
        let Some((across, down)) = lens_footprint(&self.rig, b, view, *size, i, j) else {
            return false;
        };
        let lod = across.length().max(down.length()).max(1e-30).log2();
        let lens = &self.rig.lenses[b];
        let margin = self.rig.margin_texels * lod.max(0.0).exp2();
        let c = pixel::centre(i, j);
        let ray = self.rig.ray(b, view.dir(*size, c.x, c.y));
        let Some(texel) = lens.model.project(ray) else {
            return false;
        };
        let theta = ray.x.hypot(ray.y).atan2(ray.z);
        let in_angle = (lens.theta_max - theta) * lens.texels_per_radian - margin;
        let side = f64::from(lens.side);
        let in_frame = (texel.min_element() - margin).min(side - margin - texel.max_element());
        in_angle.min(in_frame).abs() <= TIE_TEXELS
    }

    /// The pixels where `got` is more than `past` off `want` in some
    /// channel and one of the picture's hard masks ([`Self::masks`])
    /// ties there ([`Self::ties_at`]), as `(i, j)`.
    fn ties(&self, got: &[f64], want: &[f64], past: f64) -> Vec<(u32, u32)> {
        let masks = self.masks();
        if masks.is_empty() {
            return Vec::new();
        }
        let w = self.shot.1.0;
        let off = |(a, b): (&[f64], &[f64])| max_diff(a, b) > past;
        got.chunks(3)
            .zip(want.chunks(3))
            .enumerate()
            .filter(|(_, ab)| off(*ab))
            .map(|(n, _)| (n as u32 % w, n as u32 / w))
            .filter(|&(i, j)| masks.iter().any(|&b| self.ties_at(b, i, j)))
            .collect()
    }

    /// Takes each pixel of `got` that is more than `past` off `want` at a
    /// tie of the picture's masks ([`Self::ties`]) from `want`: a mask
    /// drawn in f32 on the GPU and in f64 by rung 1 may fall either way
    /// there, and the pixel is held to nothing. Gives the pixels.
    fn untie(&self, got: &mut [f64], want: &[f64], past: f64) -> Vec<(u32, u32)> {
        let ties = self.ties(got, want, past);
        let w = self.shot.1.0 as usize;
        for &(i, j) in &ties {
            let n = 3 * (j as usize * w + i as usize);
            got[n..n + 3].copy_from_slice(&want[n..n + 3]);
        }
        ties
    }

    /// [`Self::untie`] on images of one kind, the other-driver rule's
    /// way: each pixel more than `past` off at a tie taken from `want`.
    fn untie_image(&self, got: &mut Image, want: &Image, past: f64) -> Vec<(u32, u32)> {
        fn copy<T: Copy>(got: &mut [T], want: &[T], w: usize, ties: &[(u32, u32)]) {
            for &(i, j) in ties {
                let n = 3 * (j as usize * w + i as usize);
                got[n..n + 3].copy_from_slice(&want[n..n + 3]);
            }
        }
        let ties = self.ties(&unit(got), &unit(want), past);
        let w = got.width as usize;
        match (&mut got.pixels, &want.pixels) {
            (Pixels::Rgb8(g), Pixels::Rgb8(h)) => copy(g, h, w, &ties),
            (Pixels::Rgb16(g), Pixels::Rgb16(h)) => copy(g, h, w, &ties),
            (Pixels::LinearF32(g), Pixels::LinearF32(h)) => copy(g, h, w, &ties),
            _ => return Vec::new(),
        }
        ties
    }
}

/// The shots rung 1 holds, by the names the manifest gives them: the
/// equirect and the four band stripes.
fn shots() -> [(&'static str, Shot); 5] {
    [
        ("equirect", equirect()),
        (VIEW_KEYS[0], view(0)),
        (VIEW_KEYS[1], view(1)),
        (VIEW_KEYS[2], view(2)),
        (VIEW_KEYS[3], view(3)),
    ]
}

/// The seam difference's gain: mode 7 draws |g0·lin0 − g1·lin1| × 8
/// (`RigDebug::SeamDiff`).
const SEAM_DIFF_GAIN: f64 = 8.0;

/// How near its bound, in texels, a mask decides a pixel within f32's
/// rounding ([`Picture::ties_at`]). The shader works out θ, the texel and
/// the footprint's LOD in f32 and rung 1 in f64: a texel near 768 carries
/// 6.1e-5 of a texel in f32, a few such roundings in the two texels half
/// a pixel either side that 2^λ comes from move margin(λ) by up to about
/// 1e-3 of a texel at the X5-like rig's 8.5 texel margin, and a hardware
/// GPU's atan and polynomials round more loosely than lavapipe's. 1/256
/// of a texel is 1/45 000 of a radian at the lenses' 175 texels a radian.
/// Rung 2's pictures hold 0 to 5 pixels a picture that near their masks'
/// bounds; on lavapipe one of them, 1.3e-5 of a texel from its bound,
/// falls otherwise than rung 1's.
const TIE_TEXELS: f64 = 1.0 / 256.0;

/// The stitch and each block alone (debug modes 4 and 5).
const MODES: [(&str, RigDebug); 3] = [
    ("stitch", RigDebug::Off),
    ("lens_0", RigDebug::Lens0),
    ("lens_1", RigDebug::Lens1),
];

/// A pair's pictures as rung 1 holds them: each shot in each mode, read
/// back in 8 bits.
fn held(row: &'static str, frames: Pair, rig: Rig, gains: [[f64; 3]; 2]) -> Vec<Picture> {
    let mut pictures = Vec::with_capacity(shots().len() * MODES.len());
    for (shot_name, shot) in shots() {
        for (mode, debug) in MODES {
            pictures.push(Picture {
                row,
                key: format!("{shot_name}_{mode}"),
                frames,
                rig,
                gains,
                shot,
                debug,
                depth: Depth::Eight,
            });
        }
    }
    pictures
}

/// Each picture drawn on `gpu` and rendered by rung 1, the two side by
/// side.
fn both_ways(
    renderer: &mut StillRenderer,
    gpu: &Gpu,
    pictures: &[Picture],
) -> Vec<(Image, ImageF64)> {
    std::thread::scope(|s| {
        let rung1 = s.spawn(|| pictures.iter().map(Picture::rung1).collect::<Vec<_>>());
        let drawn: Vec<Image> = pictures.iter().map(|p| p.draw(renderer, gpu)).collect();
        drawn.into_iter().zip(rung1.join().unwrap()).collect()
    })
}

/// Holds each picture to the reference rule in its row, as rung 2 holds
/// it ([`Picture::held`]), its mask's ties held to nothing
/// ([`Picture::untie`]).
fn hold(rows: &mut [Row], pictures: &[Picture], drawn: &[(Image, ImageF64)]) {
    for (p, (got, want)) in pictures.iter().zip(drawn) {
        let row = rows.iter_mut().find(|r| r.name == p.row).unwrap();
        let (mut got, want) = (p.held(got), p.held_rung1(want));
        let ties = p.untie(&mut got, &want, CPU_MAX_CHANNEL);
        row.tied(&p.key, &ties);
        row.rule(&p.key, p.shot.1, &got, &want);
    }
}

/// Makes the pairs a test reads side by side, before its first picture.
fn ready_pairs(pairs: &[fn()]) {
    std::thread::scope(|s| {
        for make in pairs {
            s.spawn(make);
        }
    });
}

/// The picture `key` of `row` among `pictures`, and where it lies.
fn find<'p>(pictures: &'p [Picture], row: &str, key: &str) -> (usize, &'p Picture) {
    pictures
        .iter()
        .enumerate()
        .find(|(_, p)| p.row == row && p.key == key)
        .unwrap_or_else(|| panic!("no picture {key} of {row}"))
}

// The X5-like camera, infinitely far.

/// The X5-like camera's far pairs, each through its true rig.
fn x5_like_pictures() -> Vec<Picture> {
    let mut pictures = held(Scene::Lines.row(), Scene::Lines.frames(), x5().rig, UNIT);
    pictures.extend(held(
        Scene::Grid.row(),
        Scene::Grid.frames(),
        x5().rig,
        UNIT,
    ));
    pictures
}

/// The far `SeamLines` and `Grid` pairs: the equirect and the four band
/// stripes, the stitch and each block alone, against rung 1 by the
/// reference rule. The control yaws lens 1 by 0.5° in the GPU's rig alone
/// (4.2's band control) and draws each pair's equirect, stitched and lens
/// 1 alone: each fails the channel rule, the latter with its mask's ties
/// held to nothing as the true picture's are.
#[test]
fn rung2_x5_like_matches_rung1() {
    let _t = timed("rung2_x5_like_matches_rung1");
    ready_pairs(&[|| _ = lines(), || _ = grid()]);
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let pictures = x5_like_pictures();
    let drawn = both_ways(&mut renderer, gpu, &pictures);
    let mut rows = [
        Row::new(
            "x5_seamlines",
            "the X5-like camera's far SeamLines pair, stitched on lavapipe against rung 1's \
             picture: the 512 × 256 equirect and the four band stripes, the stitch and each \
             block alone; the control yaws lens 1 by 0.5° in the GPU's rig alone",
        ),
        Row::new(
            "x5_grid",
            "the X5-like camera's far Grid pair, as x5_seamlines holds SeamLines",
        ),
    ];
    hold(&mut rows, &pictures, &drawn);
    let yawed = Scene::Lines.turned(0);
    for row in &mut rows {
        for (key, name) in [
            ("equirect_stitch", "lens 1 yawed 0.5 in the GPU's rig"),
            ("equirect_lens_1", "lens 1 alone yawed 0.5 in the GPU's rig"),
        ] {
            let (k, p) = find(&pictures, row.name, key);
            let want = p.held_rung1(&drawn[k].1);
            let mut got = p.held(&p.draw_by(&mut renderer, gpu, &yawed, p.gains, p.depth));
            let ties = p.untie(&mut got, &want, CPU_MAX_CHANNEL);
            let (_, max, _) = row.control(name, key, p.shot.1, &got, &want);
            let off = must_fail(
                &format!(
                    "{name}°, under the channel rule, {} ties held to nothing",
                    ties.len()
                ),
                at_most(max, CPU_MAX_CHANNEL),
            );
            assert!(off > CPU_MAX_CHANNEL, "{off}");
        }
    }
    for row in rows {
        row.finish();
    }
}

// The X5-like camera, 1.5 m away.

/// The near `SeamLines` pair through its true rig, stitched at q = 1/1.5.
fn near_pictures() -> Vec<Picture> {
    let scene = Scene::NearLines;
    held(scene.row(), scene.frames(), x5().near_rig, UNIT)
}

/// The near `SeamLines` pair, stitched at q = 1/1.5 m⁻¹: rung 1's shots
/// against rung 1 by the reference rule. The control stitches the GPU's
/// equirect at q = 0, against rung 1's at q = 1/1.5, and fails the rule.
#[test]
fn rung2_near_matches_rung1() {
    let _t = timed("rung2_near_matches_rung1");
    ready_pairs(&[|| _ = near_lines()]);
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let pictures = near_pictures();
    let drawn = both_ways(&mut renderer, gpu, &pictures);
    let mut row = Row::new(
        "x5_seamlines_1m5",
        "the X5-like camera's SeamLines pair 1.5 m away, stitched at q = 1/1.5 m⁻¹ on lavapipe \
         against rung 1's picture: the 512 × 256 equirect and the four band stripes, the stitch \
         and each block alone; the control stitches the GPU's equirect at q = 0",
    );
    hold(std::slice::from_mut(&mut row), &pictures, &drawn);
    let (k, p) = find(&pictures, row.name, "equirect_stitch");
    let control = p.draw_by(&mut renderer, gpu, &x5().rig, p.gains, p.depth);
    let (_, _, rule) = row.control(
        "q 0 on the GPU's side",
        &p.key,
        p.shot.1,
        &unit(&control),
        &drawn[k].1.data,
    );
    must_fail("q = 0 on the GPU's side of the near pair", rule);
    row.finish();
}

// The Kannala–Brandt camera.

/// The Kannala–Brandt camera's `SeamLines` pair through its true rig.
fn kb_pictures() -> Vec<Picture> {
    let scene = Scene::KbLines;
    held(scene.row(), scene.frames(), kb().rig, UNIT)
}

/// The Kannala–Brandt camera's `SeamLines` pair through the shader's
/// model 1: rung 1's shots against rung 1 by the reference rule. The
/// control reads its equidistant lenses as equisolid in the GPU's rig
/// (`attribution::equisolid_for`) and draws the equirect: it fails the
/// rule.
#[test]
fn rung2_kb_rig_matches_rung1() {
    let _t = timed("rung2_kb_rig_matches_rung1");
    ready_pairs(&[|| _ = kb()]);
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let pictures = kb_pictures();
    let drawn = both_ways(&mut renderer, gpu, &pictures);
    let mut row = Row::new(
        "kb_seamlines",
        "the Kannala–Brandt camera's far SeamLines pair through fs_rig's model 1 on lavapipe \
         against rung 1's picture: the 512 × 256 equirect and the four band stripes, the stitch \
         and each block alone; the control reads the equidistant lenses as equisolid",
    );
    hold(std::slice::from_mut(&mut row), &pictures, &drawn);
    let (k, p) = find(&pictures, row.name, "equirect_stitch");
    let equisolid = Rig {
        lenses: p.rig.lenses.map(|lens| equisolid_for(&lens)),
        ..p.rig
    };
    let control = p.draw_by(&mut renderer, gpu, &equisolid, p.gains, p.depth);
    let (_, _, rule) = row.control(
        "equidistant read as equisolid",
        &p.key,
        p.shot.1,
        &unit(&control),
        &drawn[k].1.data,
    );
    must_fail("the equidistant lenses read as equisolid", rule);
    row.finish();
}

// Modes 6 and 7.

/// The 45° view of the seam on the right at 192², 4 px a degree, about
/// the lenses' own scale at the seam: `reference_rig.rs`'s
/// `right_close(192)`, where mode 7 on the far `SeamLines` pair reads
/// 1.20 at most.
fn seam_close() -> (&'static str, Shot) {
    let v = ViewParams::new(
        DQuat::from_rotation_y(-FRAC_PI_2),
        lens(Projection::Rectilinear, 45.0),
    );
    ("seam_45", (v, (192, 192)))
}

/// The weights (mode 6) and the seam difference (mode 7) of the far
/// `SeamLines` pair, read back in linear light: rung 1's shots and the
/// 45° seam view.
fn modes_pictures() -> Vec<Picture> {
    let mut pictures = Vec::with_capacity(2 * (shots().len() + 1));
    for (shot_name, shot) in shots().into_iter().chain([seam_close()]) {
        for (mode, debug) in [
            ("weights", RigDebug::Weights),
            ("seam_difference", RigDebug::SeamDiff),
        ] {
            pictures.push(Picture {
                row: "x5_seamlines_modes_6_and_7",
                key: format!("{shot_name}_{mode}"),
                frames: Scene::Lines.frames(),
                rig: x5().rig,
                gains: UNIT,
                shot,
                debug,
                depth: Depth::Linear,
            });
        }
    }
    pictures
}

/// Modes 6 and 7 on the far `SeamLines` pair at q = 0, read back in
/// linear light (`Depth::Linear`) as the nearest f16, against rung 1's,
/// which rounds them so, by the reference rule, the seam difference over
/// its gain of 8 ([`Picture::held`]): rung 1's shots and the 45° seam
/// view, where the seam difference passes 1, drawn and rendered. The
/// control reads that view's seam difference back in 8 bits and takes the
/// codes back to linear light: clipped at 1, it fails the check that the
/// difference passes 1.
#[test]
fn rung2_modes_6_and_7_match_rung1() {
    let _t = timed("rung2_modes_6_and_7_match_rung1");
    ready_pairs(&[|| _ = lines()]);
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let pictures = modes_pictures();
    let drawn = both_ways(&mut renderer, gpu, &pictures);
    let mut row = Row::new(
        "x5_seamlines_modes_6_and_7",
        "the far SeamLines pair's weights (mode 6) and seam difference (mode 7) at q = 0, read \
         back in linear light on lavapipe against rung 1's: the 512 × 256 equirect, the four \
         band stripes and the 45° seam view at 192², where the seam difference passes 1; the \
         control reads that view's seam difference back in 8 bits",
    );
    hold(std::slice::from_mut(&mut row), &pictures, &drawn);
    // Past 1 as drawn, eight times the lenses' light.
    let (k, p) = find(&pictures, row.name, "seam_45_seam_difference");
    let largest = |v: &[f64]| v.iter().copied().fold(0.0, f64::max);
    let (got, want) = (largest(&unit(&drawn[k].0)), largest(&drawn[k].1.data));
    row.measure("seam_45_seam_difference_largest", want);
    row.measure("seam_45_seam_difference_drawn_largest", got);
    for (whose, v) in [("rung 1's", want), ("the GPU's", got)] {
        let past = above(v, 1.0);
        if past.passes() != Some(true) {
            row.fail(format!(
                "{whose} seam difference on the 45° view stays at 1: {past}"
            ));
        }
    }
    let eight = p.draw_by(&mut renderer, gpu, &p.rig, p.gains, Depth::Eight);
    let linear: Vec<f64> = unit(&eight)
        .into_iter()
        .map(|v| f64::from(srgb_eotf(v as f32)))
        .collect();
    row.measure(
        "control_seam_difference_read_back_in_8_bits_largest",
        largest(&linear),
    );
    must_fail(
        "the seam difference read back in 8 bits, clipped at 1",
        above(largest(&linear), 1.0),
    );
    row.finish();
}

// Unlike gains under a reversed binding.

/// 4.2's unlike gains, by calibration block.
const GAINS: [[f64; 3]; 2] = [[1.2, 1.0, 0.9], [0.8, 1.1, 1.0]];

/// The far `SeamLines` pair under a binding that puts block 0 on track 1:
/// the X5-like rig with each block's track swapped, and the frames
/// swapped with them, so that each block still sees its own lens.
fn reversed() -> (Pair, Rig) {
    let rig = x5().rig;
    let mut lenses = rig.lenses;
    for (b, lens) in lenses.iter_mut().enumerate() {
        lens.track = 1 - lens.track;
        assert_eq!(lens.track, 1 - b);
    }
    let [front, back] = both(lines());
    ([back, front], Rig { lenses, ..rig })
}

fn gains_pictures() -> Vec<Picture> {
    let (frames, rig) = reversed();
    held("x5_seamlines_unlike_gains", frames, rig, GAINS)
}

/// The far `SeamLines` pair with unlike gains by block under a binding
/// that puts block 0 on track 1: rung 1's shots, the stitch and each
/// block alone, against rung 1 by the reference rule. The control hands
/// the GPU the gains by track, and fails the rule on the equirect.
#[test]
fn rung2_unlike_gains_under_a_reversed_binding() {
    let _t = timed("rung2_unlike_gains_under_a_reversed_binding");
    ready_pairs(&[|| _ = lines()]);
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let pictures = gains_pictures();
    let drawn = both_ways(&mut renderer, gpu, &pictures);
    let mut row = Row::new(
        "x5_seamlines_unlike_gains",
        "the far SeamLines pair with gains [[1.2, 1.0, 0.9], [0.8, 1.1, 1.0]] by block, under a \
         binding that puts block 0 on track 1, on lavapipe against rung 1's picture: the 512 × \
         256 equirect and the four band stripes, the stitch and each block alone; the control \
         hands the GPU the gains by track",
    );
    hold(std::slice::from_mut(&mut row), &pictures, &drawn);
    let (k, p) = find(&pictures, row.name, "equirect_stitch");
    let by_track = p.rig.lenses.map(|lens| GAINS[lens.track]);
    assert_ne!(by_track, GAINS);
    let control = p.draw_by(&mut renderer, gpu, &p.rig, by_track, p.depth);
    let (_, _, rule) = row.control(
        "gains by track",
        &p.key,
        p.shot.1,
        &unit(&control),
        &drawn[k].1.data,
    );
    must_fail("the gains handed over by track", rule);
    row.finish();
}

/// The ledger blesses once all of [`TESTS`] have ended well, the floors
/// are calibrated as `goldens.rs` calibrates a CPU check's, and the rule
/// fails on either clause, or on a NaN.
#[test]
fn a_blessing_waits_for_every_rung2_test() {
    assert_eq!(Blessing::after(&[]), Blessing::Waits(TESTS.to_vec()));
    let all_well: Vec<_> = TESTS.iter().map(|t| (*t, true)).collect();
    assert_eq!(Blessing::after(&all_well), Blessing::Writes);
    assert_eq!(
        Blessing::after(&all_well[..4]),
        Blessing::Waits(vec![TESTS[4]])
    );
    let mut one_failed = all_well.clone();
    one_failed[2].1 = false;
    assert_eq!(
        Blessing::after(&one_failed),
        Blessing::Refused(vec![TESTS[2]])
    );
    assert_eq!(calibrated(52.3456), 49.34);
    assert_eq!(calibrated(38.0), CPU_FLOOR_DB);
    assert_eq!(reference_rule(40.0, 0.05, 39.0), Ok(()));
    must_fail("a PSNR under its floor", reference_rule(38.9, 0.05, 39.0));
    must_fail("a channel past 0.1", reference_rule(40.0, 0.11, 39.0));
    must_fail("a NaN PSNR", reference_rule(f64::NAN, 0.05, 39.0));
    must_fail("a NaN channel", reference_rule(40.0, f64::NAN, 39.0));
}

// Rung 7's rung 2.

/// Rung 7's rung 2: every picture of the tests above on each hardware
/// GPU, against lavapipe's by D32's other-driver rule (PSNR ≥ 48 dB,
/// SSIM ≥ 0.995, at most 4 pixels more than 4 LSB off and none more than
/// 8; `judge`, as the goldens' stored images are held on another driver),
/// and against rung 1 by the reference rule at the floors blessed on
/// lavapipe, each picture held as on lavapipe: the seam difference in the
/// lenses' light, and a pixel at a tie of a hard mask held to nothing.
/// The control draws the far `SeamLines` equirect with lens 1 yawed 0.5°
/// on the GPU, against lavapipe's true picture: it fails the other-driver
/// rule.
#[test]
#[ignore = "tier2: needs a GPU (RADV or ANV)"]
fn rung2_on_radv() {
    tier2::require_hw_run();
    let began = Instant::now();
    let gpus = if AdapterChoice::from_env().is_some() {
        vec![hardware_gpu()]
    } else {
        let (gpus, seen) = hardware_gpus();
        if gpus.is_empty() {
            tier2::skip(format!("no GPU: Vulkan lists only {}", seen.join(", ")));
        }
        gpus
    };
    let pictures: Vec<Picture> = [
        x5_like_pictures(),
        near_pictures(),
        kb_pictures(),
        modes_pictures(),
        gains_pictures(),
    ]
    .concat();
    let lavapipe = gpu();
    let drawn = both_ways(&mut StillRenderer::new(lavapipe), lavapipe, &pictures);
    let mut failures = Vec::new();
    for g in &gpus {
        let name = Driver::of(g).describe();
        let mut renderer = StillRenderer::new(g);
        let mut rows: Vec<Row> = Vec::new();
        let (mut worst_psnr, mut worst_lsb) = (f64::INFINITY, 0.0f64);
        for (p, (on_lavapipe, want)) in pictures.iter().zip(&drawn) {
            let here = p.draw(&mut renderer, g);
            let (mut got, theirs) = (p.held_image(&here), p.held_image(on_lavapipe));
            p.untie_image(&mut got, &theirs, OTHER_PAST_LSB / 255.0);
            match drift(&got, &theirs) {
                Ok(d) => {
                    worst_psnr = worst_psnr.min(d.psnr);
                    worst_lsb = worst_lsb.max(d.max_lsb);
                    if let Err(e) = judge(&d, false) {
                        failures.push(format!("{} {} against lavapipe: {e}", p.row, p.key));
                    }
                }
                Err(e) => failures.push(format!("{} {}: {e}", p.row, p.key)),
            }
            if !rows.iter().any(|r| r.name == p.row) {
                rows.push(Row::reading(p.row, ""));
            }
            let row = rows.iter_mut().find(|r| r.name == p.row).unwrap();
            let (mut got, want) = (p.held(&here), p.held_rung1(want));
            let ties = p.untie(&mut got, &want, CPU_MAX_CHANNEL);
            row.tied(&p.key, &ties);
            row.rule(&p.key, p.shot.1, &got, &want);
        }
        failures.extend(rows.into_iter().flat_map(|r| r.failures));
        let (k, p) = find(&pictures, Scene::Lines.row(), "equirect_stitch");
        let yawed = p.draw_by(&mut renderer, g, &Scene::Lines.turned(0), p.gains, p.depth);
        let d = drift(&yawed, &drawn[k].0).unwrap();
        must_fail(
            "lens 1 yawed 0.5° on the GPU, against lavapipe's true picture",
            judge(&d, false),
        );
        bench::note(format!(
            "rung2_on_radv on {name}: {} pictures, the worst against lavapipe's {worst_psnr:.2} \
             dB and {worst_lsb:.0} LSB; lens 1 yawed 0.5° {:.2} dB and {:.0} LSB",
            pictures.len(),
            d.psnr,
            d.max_lsb
        ));
        for f in failures.iter_mut() {
            if !f.ends_with(']') {
                f.push_str(&format!(" [on {name}]"));
            }
        }
    }
    eprintln!(
        "rung2_on_radv: {:.3} s on {} GPU{}",
        began.elapsed().as_secs_f64(),
        gpus.len(),
        if gpus.len() == 1 { "" } else { "s" }
    );
    assert!(
        failures.is_empty(),
        "{} checks failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
