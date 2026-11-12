//! `spherecut frame` on a camera's two-lens file (M3 plan 14's frame row;
//! M3.5's unit (d)): stitched with the file's calibration and the
//! `Known` geometry through `agent::still::Still::open_pair`, on the CPU
//! reference until M3.7, with `--lens`, `--seam`, `--geometry`,
//! `--stitch-distance` and `--projection equirect`, which also renders an
//! equirect file whole.
//!
//! The files: synth's `InsvSpec::small()` (2 × 384², lossless) cut to
//! its first 6 frames, since its 30 took 0.6 s of the budget to write,
//! written once a binary, for the tests whose pictures matter;
//! and the CLI's own 2 × 128² masters of 6 frames (`tests/insv/mod.rs`),
//! each with a fault or none, for the rest and for a lens of a file
//! with no calibration. Each is written once into a folder of this
//! binary's, kept for the run. Each test runs its
//! controls beside its own checks, through `must_fail`.
//!
//! Budget (4.1 rule 5, 19.6): 1 s of the M3.5 row's 3 s, rung 3's 2 s
//! being the rest. Each test prints its time and the suite's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod document;
#[allow(dead_code)]
mod insv;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::Value;
use spherecut_agent::still::{self, Look, Options, PairOptions, Still, Stop};
use spherecut_cli::contract::frame_exit;
use spherecut_core::TimeSpec;
use spherecut_core::cancel::StopSource;
use spherecut_media::Source;
use spherecut_media::backend::SeekMode;
use spherecut_render::compare::{View, psnr};
use spherecut_render::reference::{self, RigOptions};
use spherecut_render::{Depth, Image, Pixels, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::rig::{Binding, Geometry, Tune};
use spherecut_synth::Pattern;
use spherecut_synth::code::{CornerCode, Luma, Samples, read_corner_code};
use spherecut_synth::insv::{InsvFault, InsvSpec, write_insv};
use spherecut_synth::media::{self, Codec, Spec};
use spherecut_testkit::control::must_fail;

use document::document;

/// The suite's budget in seconds (amendment 5, 19.6).
const BUDGET_S: f64 = 1.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the suite's budget. The tests run
/// side by side, so the last line printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
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
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

// The files.

/// A folder of this test binary's, kept for the run.
fn tmp() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("frame-insv-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

/// A folder of `tmp()`'s for one test's outputs.
fn outputs(name: &str) -> PathBuf {
    let d = tmp().join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `InsvSpec::small()`'s master, 2 × 384², at 6 lossless frames: frame
/// 5 is the last.
fn small_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let spec = InsvSpec {
            frames: 6,
            ..InsvSpec::small()
        };
        write_insv(&spec, &tmp().join("small")).unwrap().master
    })
}

/// The CLI's 2 × 128² master with `fault` planted, in a folder of its
/// own, written once whichever tests ask for it.
fn pair(fault: Option<InsvFault>) -> &'static Path {
    type Cells = BTreeMap<String, &'static OnceLock<PathBuf>>;
    static PAIRS: Mutex<Cells> = Mutex::new(BTreeMap::new());
    let folder = match fault {
        None => "pair".to_string(),
        Some(f) => format!("pair-{f:?}"),
    };
    let cell: &'static OnceLock<PathBuf> = PAIRS
        .lock()
        .unwrap()
        .entry(folder.clone())
        .or_insert_with(|| Box::leak(Box::new(OnceLock::new())));
    cell.get_or_init(|| insv::master(&tmp().join(folder), insv::MASTER, fault).path)
}

/// The CLI's 2 × 128² master.
fn pair_file() -> &'static Path {
    pair(None)
}

/// A stitched equirect, tagged so: synth's grid, 256 × 128, one
/// lossless frame, as `frame.rs`'s `grid_clip` makes one.
fn equirect_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let spec = Spec {
            name: "grid".into(),
            pattern: Pattern::Grid,
            width: 256,
            height: 128,
            frames: 1,
            codec: Codec::H264Lossless,
            spherical: true,
            ..Spec::default()
        };
        media::write(&spec, &tmp().join("equirect")).unwrap()
    })
}

// Running frame.

/// What a `frame --json` run gave.
#[derive(Debug)]
struct Ran {
    status: Option<i32>,
    doc: Value,
    stderr: String,
}

impl Ran {
    fn ok(&self) -> &Value {
        assert_eq!(self.status, Some(0), "{} / {}", self.doc, self.stderr);
        assert_eq!(self.doc["ok"], true, "{}", self.doc);
        &self.doc["result"]
    }

    fn code(&self) -> &str {
        self.doc["error"]["code"].as_str().unwrap_or("")
    }

    fn message(&self) -> &str {
        self.doc["error"]["message"].as_str().unwrap_or("")
    }

    /// The warnings' codes, in order.
    fn warnings(&self) -> Vec<&str> {
        self.doc["warnings"]
            .as_array()
            .map(|w| w.iter().filter_map(|w| w["code"].as_str()).collect())
            .unwrap_or_default()
    }
}

/// The binary, with nothing from the environment steering it.
fn spherecut() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    cmd.env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ADAPTER")
        .env_remove("SPHERECUT_DEBUG_FRAME_RACE")
        .env_remove("SPHERECUT_DEBUG_PANIC");
    cmd
}

/// `spherecut frame FILE -o OUT --json ARGS`, on lavapipe unless `args`
/// pick an adapter or `--reference`, and decoding in software unless
/// they pick a decoder.
fn frame(file: &Path, out: &Path, args: &[&str]) -> Ran {
    let mut cmd = spherecut();
    cmd.arg("frame").arg(file).arg("-o").arg(out).arg("--json");
    if !args
        .iter()
        .any(|a| *a == "--adapter" || *a == "--reference")
    {
        cmd.args(["--adapter", "cpu"]);
    }
    if !args.contains(&"--decoder") {
        cmd.args(["--decoder", "software"]);
    }
    let out = cmd.args(args).output().unwrap();
    Ran {
        status: out.status.code(),
        doc: document(&out.stdout),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// `spherecut probe --json FILES`' reports, in order.
fn probe(files: &[&Path]) -> Vec<Value> {
    let out = spherecut()
        .args(["probe", "--json"])
        .args(files)
        .output()
        .unwrap();
    let doc = document(&out.stdout);
    doc["result"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["report"].clone())
        .collect()
}

/// The temporary files in `dir`: there must never be any after a run.
fn temp_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| spherecut_core::fs::is_temp_name(n))
        .collect()
}

/// Runs `f` on each of `items` on a thread of its own, the results in
/// order.
fn each<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = items.iter().map(|i| s.spawn(move || f(i))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

// Pictures.

fn load(path: &Path) -> Image {
    Image::from_png(&std::fs::read(path).unwrap()).unwrap()
}

/// An 8-bit picture's samples, R, G, B a pixel.
fn rgb8(img: &Image) -> &[u8] {
    match &img.pixels {
        Pixels::Rgb8(p) => p,
        _ => panic!("not an 8-bit picture"),
    }
}

/// Whether two pictures are the same, byte for byte, saying how many
/// values differ and the first.
fn same_picture(got: &Image, want: &Image) -> Result<(), String> {
    if (got.width, got.height) != (want.width, want.height) {
        return Err(format!(
            "{}x{} against {}x{}",
            got.width, got.height, want.width, want.height
        ));
    }
    let (a, b) = (rgb8(got), rgb8(want));
    let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    match differ.first() {
        None => Ok(()),
        Some(&i) => Err(format!(
            "{} of {} values differ, the first at pixel {} channel {}: {} against {}",
            differ.len(),
            a.len(),
            i / 3,
            i % 3,
            a[i],
            b[i]
        )),
    }
}

/// The whole sphere through the turn `--yaw`, `--pitch` and `--roll`
/// give, in degrees.
fn turn(yaw: f64, pitch: f64, roll: f64) -> ViewParams {
    ViewParams::equirect(Ypr::from_degrees(yaw, pitch, roll).to_quat())
}

/// The picture the library makes of `file`'s frame `n`, stitched under
/// `pair` through `view` at `size`: `Still::open_pair` with no GPU,
/// `StillPair::seek`, and `StillPair::render`, quantized to 8 bits.
fn library(file: &Path, pair: &PairOptions, n: u64, view: ViewParams, size: (u32, u32)) -> Image {
    let src = Source::path(file);
    let stop = Stop::of(&StopSource::new());
    let mut warnings = Vec::new();
    let report = still::probe_report(src, &stop).unwrap();
    let mut opened = Still::open_pair(
        src,
        &report,
        None,
        &Options::default(),
        pair,
        &stop,
        &mut warnings,
    )
    .unwrap();
    let decoded = opened
        .seek(TimeSpec::Frame(n), SeekMode::Exact, &mut warnings)
        .unwrap();
    opened
        .render(&decoded, &[view], size, Look::default())
        .unwrap()
}

/// The file's own geometry, the entry `pair_stitching` gives it, read
/// with `binding`.
fn bound(file: &Path, binding: Binding) -> Geometry {
    let entry = still::pair_stitching(Source::path(file)).unwrap();
    Geometry {
        binding,
        ..entry.known.geometry
    }
}

/// Whether a picture's corner code, read off its luma (BT.709's
/// weights), names track `track`'s frame `n`.
fn code_is(img: &Image, track: usize, n: u32) -> Result<(), String> {
    let luma: Vec<u8> = rgb8(img)
        .chunks_exact(3)
        .map(|p| {
            let y = 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            y.round() as u8
        })
        .collect();
    let luma = Luma {
        samples: Samples::U8(&luma),
        width: img.width,
        height: img.height,
        stride: img.width as usize,
        bits: 8,
    };
    let code = read_corner_code(&luma).map_err(|e| format!("{e:?}"))?;
    if code == CornerCode::of(track, n) {
        Ok(())
    } else {
        Err(format!(
            "the code is {code:?}, not track {track}'s frame {n}"
        ))
    }
}

/// `--seam weights`' rule: red, block 0's weight, plus blue, block 1's,
/// is 255 within a code at every pixel, both lenses covering the whole
/// sphere between them.
fn weights_sum_to_one(img: &Image) -> Result<(), String> {
    let off: Vec<(usize, u8, u8)> = rgb8(img)
        .chunks_exact(3)
        .enumerate()
        .filter(|(_, p)| (i32::from(p[0]) + i32::from(p[2]) - 255).abs() > 1)
        .map(|(i, p)| (i, p[0], p[2]))
        .collect();
    match off.first() {
        None => Ok(()),
        Some((i, r, b)) => Err(format!(
            "{} pixels' red and blue don't sum to 255, the first {i}: {r} + {b}",
            off.len()
        )),
    }
}

/// `--seam difference`'s rule on an equirect picture: black wherever the
/// direction is within 60° of +Z or of −Z, where one lens alone covers,
/// and not black everywhere.
fn black_about_the_axes(img: &Image) -> Result<(), String> {
    let size = (img.width, img.height);
    let view = turn(0.0, 0.0, 0.0);
    let (mut lit_near, mut lit) = (0, 0);
    for (i, p) in rgb8(img).chunks_exact(3).enumerate() {
        let (x, y) = (i as u32 % size.0, i as u32 / size.0);
        let d = reference::sphere_dir(&view, size, f64::from(x) + 0.5, f64::from(y) + 0.5);
        if p != [0, 0, 0] {
            lit += 1;
            if d.normalize().z.abs() >= 0.5 {
                lit_near += 1;
            }
        }
    }
    if lit_near > 0 {
        return Err(format!("{lit_near} pixels within 60° of ±Z aren't black"));
    }
    if lit == 0 {
        return Err("every pixel is black".into());
    }
    Ok(())
}

/// D32's CPU rule for a picture against the CPU reference's: PSNR at
/// least 36 dB, the floor's least, and no channel more than 0.1 off.
fn within_cpu_rule(got: &Image, want: &Image) -> Result<(), String> {
    let unit =
        |img: &Image| -> Vec<f64> { rgb8(img).iter().map(|&v| f64::from(v) / 255.0).collect() };
    let (a, b) = (unit(got), unit(want));
    let (w, h) = (got.width as usize, got.height as usize);
    let db = psnr(View::new(w, h, 3, &a), View::new(w, h, 3, &b)).unwrap();
    let far = a
        .iter()
        .zip(&b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max);
    if db >= 36.0 && far <= 0.1 {
        Ok(())
    } else {
        Err(format!("PSNR {db:.2} dB, a channel {far:.3} off"))
    }
}

// The tests.

/// `frame <insv> --reference` writes the picture the library makes
/// (`Still::open_pair`, `StillPair::seek` and `render`), byte for byte,
/// under the geometry and distance the flags give; and the JSON says the
/// CPU reference rendered it, under the user's geometry, both lenses at
/// one pts.
#[test]
fn frame_insv_reference_matches_the_library() {
    let _t = timed("frame_insv_reference_matches_the_library");
    let file = small_file();
    let dir = outputs("reference");
    let common = [
        "--reference",
        "--projection",
        "equirect",
        "--size",
        "256x128",
        "--frame",
        "5",
    ];
    let runs: [(&str, &[&str]); 3] = [
        (
            "asked",
            &["--geometry", "binding=direct", "--stitch-distance", "inf"],
        ),
        ("without --geometry", &["--stitch-distance", "inf"]),
        (
            "at 2 m",
            &["--geometry", "binding=direct", "--stitch-distance", "2m"],
        ),
    ];
    let (want, ran) = std::thread::scope(|s| {
        let want = s.spawn(|| {
            let pair = PairOptions {
                geometry: Some(bound(file, Binding::Direct)),
                tune: Tune {
                    q_per_m: 0.0,
                    ..Tune::default()
                },
                rig: RigOptions::default(),
            };
            library(file, &pair, 5, turn(0.0, 0.0, 0.0), (256, 128))
        });
        let ran = each(&runs, |(name, extra)| {
            let mut args = common.to_vec();
            args.extend_from_slice(extra);
            let out = dir.join(format!("{name}.png"));
            (frame(file, &out, &args), out)
        });
        (want.join().unwrap(), ran)
    });
    let (asked, out) = &ran[0];
    let r = asked.ok();
    assert_eq!(r["renderer"], "reference", "{r}");
    assert_eq!(r["gpu"], Value::Null, "{r}");
    assert_eq!(r["view"]["projection"], "equirect", "{r}");
    assert_eq!(r["view"]["fov"], 360.0, "{r}");
    let stitch = &r["stitch"];
    assert_eq!(stitch["geometry"]["status"], "user", "{r}");
    assert_eq!(stitch["distance_m"], Value::Null, "{r}");
    let pts: Vec<&Value> = stitch["lenses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| &l["pts"])
        .collect();
    assert_eq!(pts.len(), 2, "{r}");
    assert_eq!(pts[0], pts[1], "{r}");
    assert_eq!(pts[0], &r["frame"]["pts"], "{r}");
    assert_eq!(r["frame"]["time"]["frame"], 5, "{r}");
    same_picture(&load(out), &want).unwrap();
    // The controls: the file's own geometry (the X5 row's Reversed), and
    // the default distance, each a picture of its own.
    for (k, (name, _)) in runs.iter().enumerate().skip(1) {
        let (ran, out) = &ran[k];
        let r = ran.ok();
        if k == 1 {
            assert_eq!(r["stitch"]["geometry"]["status"], "provisional", "{r}");
        } else {
            assert_eq!(r["stitch"]["distance_m"], 2.0, "{r}");
        }
        must_fail(name, same_picture(&load(out), &want));
    }
}

/// Whether a run is the refusal until M3.7: `not_equirect`, its exit,
/// both hints (`--reference` for the CPU's stitch, and M3.7 for the
/// GPU's) beside `--equirect`'s, and nothing written to `out` or beside
/// it.
fn refused_until_m3_7(ran: &Ran, out: &Path) -> Result<(), String> {
    if ran.code() != "not_equirect" {
        return Err(format!("{} / {}", ran.doc, ran.stderr));
    }
    if ran.status != Some(frame_exit("not_equirect") as i32) {
        return Err(format!("exit {:?}", ran.status));
    }
    for hint in ["--reference", "M3.7", "--equirect"] {
        if !ran.message().contains(hint) {
            return Err(format!("no {hint:?} in {:?}", ran.message()));
        }
    }
    if out.exists() || !temp_files(out.parent().unwrap()).is_empty() {
        return Err("something was written".into());
    }
    Ok(())
}

/// Without `--reference`, `frame <insv>` fails with a clear error until
/// M3.7 stitches on the GPU, and writes nothing; `--equirect` still
/// renders the file's first stream as M1 did.
#[test]
fn frame_insv_needs_reference_until_m3_7() {
    let _t = timed("frame_insv_needs_reference_until_m3_7");
    let file = pair_file();
    let dir = outputs("needs-reference");
    let cases: [(&str, &[&str]); 3] = [
        ("gpu", &["--size", "64x32"]),
        ("equirect", &["--size", "64x32", "--equirect"]),
        ("reference", &["--size", "64x32", "--reference"]),
    ];
    let ran = each(&cases, |(name, args)| {
        let out = dir.join(format!("{name}.png"));
        (frame(file, &out, args), out)
    });
    let (gpu, out) = &ran[0];
    refused_until_m3_7(gpu, out).unwrap();
    // --equirect: the first stream as an equirect, on the GPU, no stitch.
    let r = ran[1].0.ok();
    assert_eq!(r["renderer"], "gpu", "{r}");
    assert_eq!(r["source"]["stream"], 0, "{r}");
    assert_eq!(r["stitch"], Value::Null, "{r}");
    // The control: --reference stitches.
    let (reference, out) = &ran[2];
    assert_eq!(reference.ok()["renderer"], "reference");
    must_fail("--reference", refused_until_m3_7(reference, out));
}

/// `--lens 0` and `--lens 1` show one lens raw, its corner code naming
/// its track and frame, with or without a calibration; `--seam weights`
/// and `--seam difference` show the stitch's weights and seam difference
/// (`RigDebug`'s modes 6 and 7).
#[test]
fn frame_insv_shows_a_lens_and_the_seam() {
    let _t = timed("frame_insv_shows_a_lens_and_the_seam");
    let dir = outputs("lens-and-seam");
    let seams = ["weights", "difference"];
    let (lens_cases, lenses, seamed) = std::thread::scope(|s| {
        let seamed = s.spawn(|| {
            each(&seams, |seam| {
                let out = dir.join(format!("seam-{seam}.png"));
                let ran = frame(
                    small_file(),
                    &out,
                    &[
                        "--reference",
                        "--projection",
                        "equirect",
                        "--size",
                        "256x128",
                        "--seam",
                        seam,
                    ],
                );
                (ran, out)
            })
        });
        let files = [
            (small_file(), "small"),
            (pair(Some(InsvFault::NoCalibration)), "no-calibration"),
        ];
        let lens_cases: Vec<(&Path, &str, usize)> = files
            .into_iter()
            .flat_map(|(f, name)| [(f, name, 0), (f, name, 1)])
            .collect();
        let lenses = each(&lens_cases, |(file, name, n)| {
            let out = dir.join(format!("lens-{name}-{n}.png"));
            let lens = n.to_string();
            let ran = frame(
                file,
                &out,
                &[
                    "--reference",
                    "--lens",
                    &lens,
                    "--size",
                    "384x384",
                    "--frame",
                    "5",
                ],
            );
            (ran, out)
        });
        (lens_cases, lenses, seamed.join().unwrap())
    });
    for ((_, name, n), (ran, out)) in lens_cases.iter().zip(&lenses) {
        let r = ran.ok();
        assert_eq!(r["renderer"], "reference", "{r}");
        assert_eq!(r["source"]["stream"], *n, "{name}: {r}");
        assert_eq!(r["stitch"], Value::Null, "{name}: {r}");
        code_is(&load(out), *n, 5).unwrap_or_else(|e| panic!("{name}, --lens {n}: {e}"));
    }
    // The control: lens 0's picture isn't track 1's.
    must_fail("--lens 0 as track 1", code_is(&load(&lenses[0].1), 1, 5));

    let [(weights, weights_out), (difference, difference_out)] = &seamed[..] else {
        unreachable!()
    };
    weights.ok();
    difference.ok();
    let (weights, difference) = (load(weights_out), load(difference_out));
    weights_sum_to_one(&weights).unwrap();
    must_fail("--seam difference's sum", weights_sum_to_one(&difference));
    black_about_the_axes(&difference).unwrap();
    must_fail(
        "--seam weights about the axes",
        black_about_the_axes(&weights),
    );
}

/// `--geometry` takes each of `Geometry`'s fields by its enum's names,
/// and `--stitch-distance` metres or `inf`; a bad key or value is a usage
/// error naming it; and each of `--lens`, `--seam`, `--geometry` and
/// `--stitch-distance` is a usage error with `--project`, `--equirect`
/// or `--stream`, or on a file that isn't a camera's pair, as `--lens`
/// is with any of the other three. The control: binding=direct and
/// binding=reversed render different pictures.
#[test]
fn frame_insv_takes_geometry_and_distance() {
    let _t = timed("frame_insv_takes_geometry_and_distance");
    let dir = outputs("geometry");
    let missing = dir.join("missing.insv");
    let out = dir.join("never.png");
    // Every value of every key, and four distances: each parses, so the
    // run goes on to the file, which isn't there.
    let parsed: [&[&str]; 4] = [
        &[
            "--geometry",
            "binding=direct,canvas=a,turn=y,tz=as-stored,upright=zero,generation=v3",
            "--stitch-distance",
            "inf",
        ],
        &[
            "--geometry",
            "binding=reversed,canvas=b,turn=x,tz=negated,upright=plus90,generation=v6",
            "--stitch-distance",
            "2m",
        ],
        &[
            "--geometry",
            "canvas=c,upright=minus90",
            "--stitch-distance",
            "0.3m",
        ],
        &["--geometry", "upright=half", "--stitch-distance", "12.5m"],
    ];
    // Each bad one, and the words its message must name.
    let bad: [(&[&str], &[&str]); 8] = [
        (
            &["--geometry", "binding=sideways"],
            &["binding", "sideways"],
        ),
        (&["--geometry", "colour=red"], &["colour"]),
        (&["--geometry", "binding"], &["binding"]),
        (&["--geometry", "canvas=d,binding=direct"], &["canvas", "d"]),
        (&["--geometry", "turn=y,turn=x"], &["turn"]),
        (&["--stitch-distance", "2"], &["--stitch-distance", "2"]),
        (&["--stitch-distance", "0.1m"], &["0.3"]),
        (&["--stitch-distance", "-2m"], &["-2m"]),
    ];
    // The conflicts: every new flag with each flag it can't go with.
    let new: [&[&str]; 4] = [
        &["--lens", "0"],
        &["--seam", "weights"],
        &["--geometry", "binding=direct"],
        &["--stitch-distance", "2m"],
    ];
    let mut conflicts: Vec<Vec<&str>> = Vec::new();
    for flag in new {
        for with in [&["--equirect"][..], &["--stream", "0"][..]] {
            conflicts.push([flag, with].concat());
        }
    }
    for flag in &new[1..] {
        conflicts.push([new[0], *flag].concat());
    }
    let projects: Vec<Vec<&str>> = new.iter().map(|f| f.to_vec()).collect();
    let project = |args: &Vec<&str>| {
        let o = spherecut()
            .args(["frame", "--project"])
            .arg(dir.join("p.spherecut"))
            .arg("-o")
            .arg(&out)
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        Ran {
            status: o.status.code(),
            doc: document(&o.stdout),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    };
    let (parsed_runs, bad_runs, conflict_runs, project_runs, plain_runs, pictures) =
        std::thread::scope(|s| {
            let parsed_runs = s.spawn(|| each(&parsed, |args| frame(&missing, &out, args)));
            let bad_runs = s.spawn(|| each(&bad, |(args, _)| frame(&missing, &out, args)));
            let conflict_runs = s.spawn(|| each(&conflicts, |args| frame(pair_file(), &out, args)));
            let project_runs = s.spawn(|| each(&projects, project));
            let plain_runs = s.spawn(|| {
                each(&new, |args| {
                    let mut all = vec!["--reference"];
                    all.extend_from_slice(args);
                    frame(equirect_file(), &out, &all)
                })
            });
            let pictures = each(&["direct", "reversed"], |binding| {
                let png = dir.join(format!("{binding}.png"));
                let geometry = format!("binding={binding}");
                let ran = frame(
                    pair_file(),
                    &png,
                    &[
                        "--reference",
                        "--projection",
                        "equirect",
                        "--size",
                        "64x32",
                        "--geometry",
                        &geometry,
                    ],
                );
                (ran, png)
            });
            (
                parsed_runs.join().unwrap(),
                bad_runs.join().unwrap(),
                conflict_runs.join().unwrap(),
                project_runs.join().unwrap(),
                plain_runs.join().unwrap(),
                pictures,
            )
        });
    for (args, ran) in parsed.iter().zip(&parsed_runs) {
        assert_eq!(
            ran.code(),
            "not_found",
            "{args:?}: {} {}",
            ran.doc,
            ran.stderr
        );
    }
    for ((args, names), ran) in bad.iter().zip(&bad_runs) {
        assert_eq!(ran.status, Some(2), "{args:?}: {}", ran.doc);
        assert_eq!(ran.code(), "usage", "{args:?}: {}", ran.doc);
        for name in *names {
            assert!(
                ran.message().contains(name),
                "{args:?}: no {name:?} in {:?}",
                ran.message()
            );
        }
    }
    for (args, ran) in conflicts
        .iter()
        .chain(&projects)
        .zip(conflict_runs.iter().chain(&project_runs))
    {
        assert_eq!(ran.status, Some(2), "{args:?}: {}", ran.doc);
        assert_eq!(ran.code(), "usage", "{args:?}: {}", ran.doc);
    }
    for (args, ran) in new.iter().zip(&plain_runs) {
        assert_eq!(ran.status, Some(2), "{args:?}: {}", ran.doc);
        assert_eq!(ran.code(), "usage", "{args:?}: {}", ran.doc);
        assert!(
            ran.message().contains("camera's pair"),
            "{args:?}: {}",
            ran.message()
        );
    }
    let [(direct, direct_out), (reversed, reversed_out)] = &pictures[..] else {
        unreachable!()
    };
    for ran in [direct, reversed] {
        let r = ran.ok();
        assert_eq!(r["stitch"]["geometry"]["status"], "user", "{r}");
        assert!(
            r["stitch"]["geometry"]["evidence"]
                .as_str()
                .unwrap()
                .contains("--geometry"),
            "{r}"
        );
    }
    must_fail(
        "binding=direct against binding=reversed",
        same_picture(&load(direct_out), &load(reversed_out)),
    );
}

/// Whether a run is a clear error for a trailer at fault: the exit the
/// exit table gives its code, no picture or temporary file, no panic,
/// and `names` in its message.
fn clear_error(ran: &Ran, out: &Path, names: &[&str]) -> Result<(), String> {
    if ran.status == Some(0) || ran.doc["ok"] != false {
        return Err(format!("it rendered: {}", ran.doc));
    }
    if ran.status != Some(frame_exit(ran.code()) as i32) || ran.code() == "internal" {
        return Err(format!("exit {:?} for {}", ran.status, ran.doc));
    }
    if ran.stderr.contains("panicked") {
        return Err(format!("a panic: {}", ran.stderr));
    }
    if out.exists() || !temp_files(out.parent().unwrap()).is_empty() {
        return Err("something was written".into());
    }
    for name in names {
        if !ran.message().contains(name) {
            return Err(format!("no {name:?} in {:?}", ran.message()));
        }
    }
    Ok(())
}

/// Whether probe's report warns `bad_trailer`.
fn warns_bad_trailer(report: &Value) -> Result<(), String> {
    let warned = report["warnings"]
        .as_array()
        .is_some_and(|w| w.iter().any(|w| w["code"] == "bad_trailer"));
    if warned {
        Ok(())
    } else {
        Err(format!("no bad_trailer in {}", report["warnings"]))
    }
}

/// Each trailer `InsvFault` (5.5) on `write_insv`'s files: probe warns
/// `bad_trailer`, and `frame --reference` fails by the exit table with
/// no picture and no panic, its message naming the record or field at
/// fault. Under `BadMagic` probe doesn't call the file Insta360's, so
/// frame never reads the trailer, and its refusal carries probe's
/// warning; under `NoCalibration` it says "no calibration". The control:
/// the file without a fault fails each check.
#[test]
fn frame_insv_trailer_faults_are_clear_errors() {
    let _t = timed("frame_insv_trailer_faults_are_clear_errors");
    let dir = outputs("faults");
    // Each fault, and the words that name what is at fault.
    let faults: [(InsvFault, &[&str]); 4] = [
        (InsvFault::TruncatedTrailer, &["tail"]),
        (InsvFault::IndexSlotPastEnd, &["record 1"]),
        (InsvFault::WrongTokenCount, &["calibration", "tokens"]),
        (InsvFault::VarintOverflow, &["record 1", "varint"]),
    ];
    let all: Vec<Option<InsvFault>> = faults
        .iter()
        .map(|(f, _)| Some(*f))
        .chain([
            Some(InsvFault::BadMagic),
            Some(InsvFault::NoCalibration),
            None,
        ])
        .collect();
    let files = each(&all, |f| pair(*f));
    let ran = each(&files, |file| {
        let name = file.parent().unwrap().file_name().unwrap().to_owned();
        let out = dir.join(&name).join("out.png");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        (frame(file, &out, &["--reference", "--size", "64x32"]), out)
    });
    let reports = probe(&files);
    let unfaulted = all.len() - 1;
    for (k, (fault, names)) in faults.iter().enumerate() {
        warns_bad_trailer(&reports[k]).unwrap_or_else(|e| panic!("{fault:?}: {e}"));
        let (r, out) = &ran[k];
        clear_error(r, out, names).unwrap_or_else(|e| panic!("{fault:?}: {e}"));
        assert_eq!(r.code(), "not_equirect", "{fault:?}: {}", r.doc);
        eprintln!("{fault:?}: {}", r.message());
        // The controls: the file without a fault.
        let (r, out) = &ran[unfaulted];
        must_fail(
            &format!("{fault:?}'s probe check, unfaulted"),
            warns_bad_trailer(&reports[unfaulted]),
        );
        must_fail(
            &format!("{fault:?}'s frame check, unfaulted"),
            clear_error(r, out, names),
        );
    }
    // BadMagic: not Insta360's to probe, which warns; frame's refusal
    // carries the warning.
    let k = faults.len();
    warns_bad_trailer(&reports[k]).unwrap();
    assert_ne!(reports[k]["camera"]["layout"], "insta360_dual_fisheye");
    let (r, out) = &ran[k];
    clear_error(r, out, &[]).unwrap();
    assert_eq!(r.code(), "not_equirect", "{}", r.doc);
    assert!(r.warnings().contains(&"bad_trailer"), "{}", r.doc);
    eprintln!("BadMagic: {}", r.message());
    must_fail(
        "BadMagic's warning, unfaulted",
        if ran[unfaulted].0.warnings().contains(&"bad_trailer") {
            Ok(())
        } else {
            Err(())
        },
    );
    // NoCalibration.
    let (r, out) = &ran[k + 1];
    clear_error(r, out, &["no calibration"]).unwrap();
    eprintln!("NoCalibration: {}", r.message());
    must_fail(
        "NoCalibration's check, unfaulted",
        clear_error(&ran[unfaulted].0, &ran[unfaulted].1, &["no calibration"]),
    );
}

/// `--projection equirect`, which M3.5 adds for `.insv` files, on a file
/// that is already a stitched equirect: the whole sphere through the
/// keyed turn, as `reference::render` draws it through
/// `ViewParams::equirect`; on lavapipe within D32's CPU rule of it; with
/// `--fov` or `--morph`, and `--morph-to equirect` on any projection, a
/// usage error.
#[test]
fn frame_projection_equirect_on_an_equirect_file() {
    let _t = timed("frame_projection_equirect_on_an_equirect_file");
    let file = equirect_file();
    let dir = outputs("projection-equirect");
    let size = (256, 128);
    let runs: [(&str, &[&str]); 7] = [
        ("yaw30", &["--reference", "--yaw", "30"]),
        ("yaw0", &["--reference"]),
        ("lavapipe", &["--adapter", "cpu", "--yaw", "30"]),
        ("fov", &["--reference", "--fov", "90"]),
        (
            "morph",
            &["--reference", "--morph-to", "fisheye", "--morph", "0.5"],
        ),
        (
            "morph-to-equirect",
            &[
                "--reference",
                "--morph-to",
                "equirect",
                "--morph",
                "0.5",
                "--projection",
                "rectilinear",
            ],
        ),
        (
            "fisheye-to-equirect",
            &[
                "--reference",
                "--morph-to",
                "equirect",
                "--morph",
                "1",
                "--projection",
                "fisheye",
            ],
        ),
    ];
    let (want, want_yaw0, ran) = std::thread::scope(|s| {
        // The reference's picture of the decoded frame, through the turn.
        let want = s.spawn(|| {
            let src = Source::path(file);
            let stop = Stop::of(&StopSource::new());
            let mut warnings = Vec::new();
            let probed = still::probe(src, &Options::default(), &stop, &mut warnings).unwrap();
            let mut opened = Still::open(
                src,
                &probed,
                None,
                &Options::default(),
                &stop,
                &mut warnings,
            )
            .unwrap();
            let decoded = opened
                .seek(TimeSpec::Frame(0), SeekMode::Exact, &mut warnings)
                .unwrap();
            let opts = reference::Options {
                level0: false,
                placement: opened.placement(),
            };
            let draw = |view: ViewParams| {
                let picture =
                    reference::render(decoded.frame.as_ref(), &view, size, &opts).unwrap();
                still::quantize(&picture, Depth::Eight)
            };
            (draw(turn(30.0, 0.0, 0.0)), draw(turn(0.0, 0.0, 0.0)))
        });
        let ran = each(&runs, |(name, extra)| {
            let mut args = vec!["--size", "256x128"];
            if !extra.contains(&"--projection") {
                args.extend(["--projection", "equirect"]);
            }
            args.extend_from_slice(extra);
            let out = dir.join(format!("{name}.png"));
            (frame(file, &out, &args), out)
        });
        let (want, want_yaw0) = want.join().unwrap();
        (want, want_yaw0, ran)
    });
    let (yaw30, out) = &ran[0];
    let r = yaw30.ok();
    assert_eq!(r["view"]["projection"], "equirect", "{r}");
    assert_eq!(r["view"]["fov"], 360.0, "{r}");
    assert_eq!(r["view"]["yaw"], 30.0, "{r}");
    assert_eq!(r["view"]["morph_to"], Value::Null, "{r}");
    assert_eq!(r["stitch"], Value::Null, "{r}");
    let got = load(out);
    same_picture(&got, &want).unwrap();
    // The control: --yaw 0's picture is another.
    let (yaw0, out) = &ran[1];
    yaw0.ok();
    let yaw0 = load(out);
    same_picture(&yaw0, &want_yaw0).unwrap();
    must_fail("--yaw 0 as --yaw 30", same_picture(&yaw0, &want));
    // On lavapipe, within D32's CPU rule; --yaw 0's reference isn't.
    let (gpu, out) = &ran[2];
    assert_eq!(gpu.ok()["renderer"], "gpu");
    let gpu = load(out);
    within_cpu_rule(&gpu, &want).unwrap();
    must_fail(
        "lavapipe against --yaw 0",
        within_cpu_rule(&gpu, &want_yaw0),
    );
    // The usage errors.
    for ((name, _), (ran, _)) in runs.iter().zip(&ran).skip(3) {
        assert_eq!(ran.status, Some(2), "{name}: {}", ran.doc);
        assert_eq!(ran.code(), "usage", "{name}: {}", ran.doc);
    }
}
