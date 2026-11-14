//! `spherecut frame` on a camera's two-lens file (M3 plan 14's frame row;
//! M3.5's unit (d), M3.7's unit (d)): stitched with the file's
//! calibration and the `Known` geometry through
//! `agent::still::Still::open_pair`, on the GPU (lavapipe here) by
//! default and on the CPU reference under `--reference`, with `--lens`,
//! `--seam`, `--geometry`, `--stitch-distance` and `--projection
//! equirect`, which also renders an equirect file whole.
//!
//! The files: synth's `InsvSpec::small()` (2 × 384², 30 lossless
//! frames), written once a binary, for the tests whose pictures matter;
//! and the CLI's own 2 × 128² masters of 6 frames (`tests/insv/mod.rs`),
//! each with a fault or none, for the rest and for a lens of a file
//! with no calibration. Each is written once into a folder of this
//! binary's, kept for the run and removed by the next run once this one
//! has ended. Each test runs its controls beside its own checks, through
//! `must_fail`. The `--reference` runs the GPU's pictures are held to are
//! run once a binary ([`reference_equirect`], [`reference_lens0`]),
//! whichever test asks first.
//!
//! Budget (4.1 rule 5, 19.6): 1.9 s, the binary's measured time on the
//! cloud host with the GPU's runs (1.74 to 1.84 s; 1.56 to 1.59 s before
//! M3.7 added them), within the 2.5 s M3.7 allows it. Each test prints
//! its time and the suite's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod document;
#[allow(dead_code)]
mod insv;
mod lavapipe;

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
use spherecut_media::backend::SeekMode;
use spherecut_media::{Source, mp4};
use spherecut_render::compare::{View, psnr, sample_diff};
use spherecut_render::reference::{self, RigOptions};
use spherecut_render::{Depth, Image, Pixels, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::rig::{Binding, Geometry, Tune};
use spherecut_synth::Pattern;
use spherecut_synth::code::{CornerCode, Luma, Samples, read_corner_code};
use spherecut_synth::insv::{Field, InsvFault, InsvSpec, write_insv};
use spherecut_synth::media::{self, Codec, Spec};
use spherecut_testkit::control::must_fail;

use document::document;

/// The suite's budget in seconds (amendment 5, 19.6).
const BUDGET_S: f64 = 1.9;

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

/// The start of the name of each run's folder, before its process id.
const RUN: &str = "frame-insv-";

/// A folder of this test binary's, kept for the run. The folders of
/// runs that have ended go first, so that they don't pile up.
fn tmp() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
        sweep(root);
        let d = root.join(format!("{RUN}{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

/// Removes each folder in `root` named [`RUN`] and the id of a process
/// that is no longer running: a run that has ended, whose files no test
/// will read again.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let pid = name
            .to_str()
            .and_then(|n| n.strip_prefix(RUN))
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid
            && !Path::new(&format!("/proc/{pid}")).exists()
        {
            std::fs::remove_dir_all(e.path()).ok();
        }
    }
}

/// A folder of `tmp()`'s for one test's outputs.
fn outputs(name: &str) -> PathBuf {
    let d = tmp().join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `InsvSpec::small()`'s master: 2 × 384², 30 lossless frames. It
/// prints how long it took to write, that share of the budget.
fn small_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let began = Instant::now();
        let master = write_insv(&InsvSpec::small(), &tmp().join("small"))
            .unwrap()
            .master;
        eprintln!(
            "InsvSpec::small(): written in {:.3} s",
            began.elapsed().as_secs_f64()
        );
        master
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

/// The CLI's master with its silent AAC track's `mdhd` timescale set to
/// 0: damage outside the lens tracks, which probe finds ("track #2: its
/// `mdhd` gives timescale 0") and FFmpeg opens past, taking 1. The
/// trailer is the master's, intact.
fn damaged_pair() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let clean = insv::tracks(pair_file().parent().unwrap(), insv::MASTER);
        let mut bytes = std::fs::read(&clean).unwrap();
        let tree = mp4::read_file(&clean).unwrap();
        let mdhd = tree
            .tracks()
            .find(|t| mp4::handler_type(t).is_some_and(|h| h.0 == *b"soun"))
            .and_then(|t| t.path(&[b"mdia", b"mdhd"]))
            .expect("the AAC track's mdhd");
        // After the version, flags, and creation and modification times.
        let times = if mdhd.payload.first() == Some(&1) {
            16
        } else {
            8
        };
        let at = usize::try_from(mdhd.offset + mdhd.header_len).unwrap() + 4 + times;
        bytes[at..at + 4].fill(0);
        let dir = tmp().join("pair-damaged");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(insv::tracks(&dir, insv::MASTER), &bytes).unwrap();
        insv::retrailed(&dir, insv::MASTER, insv::MASTER, &insv::records(), None).path
    })
}

/// The CLI's master on the same tracks, its trailer naming a model the
/// table has no entry for: field 2 "Insta360 X9".
fn unlisted_pair() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let mut records = insv::records();
        let model = records.metadata.iter_mut().find(|f| f.number == 2);
        *model.unwrap() = Field::string(2, "Insta360 X9");
        let dir = tmp().join("pair-unlisted");
        std::fs::create_dir_all(&dir).unwrap();
        let tracks = insv::tracks(pair_file().parent().unwrap(), insv::MASTER);
        std::fs::copy(&tracks, insv::tracks(&dir, insv::MASTER)).unwrap();
        insv::retrailed(&dir, insv::MASTER, insv::MASTER, &records, None).path
    })
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
    frame_in(file, out, args, &[])
}

/// [`frame`] with `env` set.
fn frame_in(file: &Path, out: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Ran {
    let mut cmd = spherecut();
    cmd.envs(env.iter().copied());
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

/// A picture's samples in unit scale.
fn unit(img: &Image) -> Vec<f64> {
    match &img.pixels {
        Pixels::Rgb8(p) => p.iter().map(|&v| f64::from(v) / 255.0).collect(),
        Pixels::Rgb16(p) => p.iter().map(|&v| f64::from(v) / 65535.0).collect(),
        Pixels::LinearF32(p) => p.iter().map(|&v| f64::from(v)).collect(),
    }
}

/// D32's CPU rule for a picture against the CPU reference's: PSNR at
/// least 36 dB, the floor's least, and no channel more than 0.1 off, as
/// `frame.rs`'s `within_cpu_rule` holds one frame.
fn within_cpu_rule(got: &Image, want: &Image) -> Result<(), String> {
    if (got.width, got.height) != (want.width, want.height) {
        return Err(format!(
            "{}x{} against {}x{}",
            got.width, got.height, want.width, want.height
        ));
    }
    let (a, b) = (unit(got), unit(want));
    let (w, h) = (got.width as usize, got.height as usize);
    let db = psnr(View::new(w, h, 3, &a), View::new(w, h, 3, &b)).unwrap();
    let far = a
        .iter()
        .zip(&b)
        .map(|(x, y)| sample_diff(*x, *y))
        .fold(0.0, f64::max);
    if db >= 36.0 && far <= 0.1 {
        Ok(())
    } else {
        Err(format!("PSNR {db:.2} dB, a channel {far:.3} off"))
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
/// sphere between them on an equirect finer than about 2.5° a pixel, as
/// 512 × 256 is (amendment 5, D64); a coarser one leaves a gap at the
/// seam that no lens covers, where both are 0.
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

// The runs the GPU's are held to.

/// The stitched equirect's flags: `small()`'s frame 5, the whole sphere
/// at 512 × 256, under binding=direct, infinitely far.
const STITCHED: [&str; 10] = [
    "--projection",
    "equirect",
    "--size",
    "512x256",
    "--geometry",
    "binding=direct",
    "--stitch-distance",
    "inf",
    "--frame",
    "5",
];

/// `--lens 0`'s flags: `small()`'s track 0 at frame 5, as coded.
const LENS0: [&str; 6] = ["--lens", "0", "--size", "384x384", "--frame", "5"];

/// `frame --reference` of `small()` with [`STITCHED`]'s flags, run once a
/// binary: `frame_insv_reference_matches_the_library` holds it to the
/// library, and `frame_insv_stitches_on_the_gpu` the GPU's to it.
fn reference_equirect() -> &'static (Ran, PathBuf) {
    static RAN: OnceLock<(Ran, PathBuf)> = OnceLock::new();
    RAN.get_or_init(|| {
        let out = outputs("reference").join("asked.png");
        let mut args = vec!["--reference"];
        args.extend_from_slice(&STITCHED);
        (frame(small_file(), &out, &args), out)
    })
}

/// `frame --reference --lens 0` of `small()`, run once a binary:
/// `frame_insv_shows_a_lens_and_the_seam` reads its corner code, and
/// `frame_insv_stitches_on_the_gpu` holds the GPU's to it.
fn reference_lens0() -> &'static (Ran, PathBuf) {
    static RAN: OnceLock<(Ran, PathBuf)> = OnceLock::new();
    RAN.get_or_init(|| {
        let out = outputs("lens-and-seam").join("lens-small-0.png");
        let mut args = vec!["--reference"];
        args.extend_from_slice(&LENS0);
        (frame(small_file(), &out, &args), out)
    })
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
        "512x256",
        "--frame",
        "5",
    ];
    // The first, asked for, is reference_equirect's run.
    let runs: [(&str, &[&str]); 3] = [
        ("asked", &[]),
        ("without --geometry", &["--stitch-distance", "inf"]),
        ("at the default distance", &["--geometry", "binding=direct"]),
    ];
    let (want, asked, ran) = std::thread::scope(|s| {
        let want = s.spawn(|| {
            let pair = PairOptions {
                geometry: Some(bound(file, Binding::Direct)),
                tune: Tune {
                    q_per_m: 0.0,
                    ..Tune::default()
                },
                rig: RigOptions::default(),
            };
            library(file, &pair, 5, turn(0.0, 0.0, 0.0), (512, 256))
        });
        let asked = s.spawn(reference_equirect);
        let ran = each(&runs[1..], |(name, extra)| {
            let mut args = common.to_vec();
            args.extend_from_slice(extra);
            let out = dir.join(format!("{name}.png"));
            (frame(file, &out, &args), out)
        });
        (want.join().unwrap(), asked.join().unwrap(), ran)
    });
    let (asked, out) = asked;
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
    // the default distance, 2 m, with no --stitch-distance, each a
    // picture of its own.
    for (k, (name, _)) in runs.iter().enumerate().skip(1) {
        let (ran, out) = &ran[k - 1];
        let r = ran.ok();
        if k == 1 {
            assert_eq!(r["stitch"]["geometry"]["status"], "provisional", "{r}");
        } else {
            assert_eq!(r["stitch"]["distance_m"], 2.0, "{r}");
        }
        must_fail(name, same_picture(&load(out), &want));
    }
}

/// Whether a run is the refusal of a pair whose calibration can't be
/// read: `not_equirect` by its exit, "no calibration" in the message
/// with the hint that `--lens 0|1` shows each lens, never the refusal
/// M3.5 gave until M3.7 ("the GPU's stitch arrives in M3.7"), and
/// nothing written to `out` or beside it.
fn refused_for_its_calibration(ran: &Ran, out: &Path) -> Result<(), String> {
    if ran.code() != "not_equirect" {
        return Err(format!("{} / {}", ran.doc, ran.stderr));
    }
    if ran.status != Some(frame_exit("not_equirect") as i32) {
        return Err(format!("exit {:?}", ran.status));
    }
    for words in ["no calibration", "--lens 0|1"] {
        if !ran.message().contains(words) {
            return Err(format!("no {words:?} in {:?}", ran.message()));
        }
    }
    if ran.message().contains("arrives in M3.7") {
        return Err(format!("M3.5's words in {:?}", ran.message()));
    }
    if out.exists() || !temp_files(out.parent().unwrap()).is_empty() {
        return Err("something was written".into());
    }
    Ok(())
}

/// Without `--reference`, `frame <insv>` stitches on the GPU (lavapipe):
/// `small()`'s frame 5 under binding=direct, infinitely far, within D32's
/// CPU rule of `--reference`'s picture of it (`reference_equirect`), with
/// renderer `gpu`, the adapter, and both lenses at the reference's pts;
/// `--lens 0` within the rule of `--reference --lens 0`'s; and `--seam
/// weights`, whose red and blue sum to 255. A pair whose calibration
/// can't be read is `not_equirect`, "no calibration", with the hint that
/// `--lens 0|1` shows each lens, and `--equirect` renders the first
/// stream as M1 did. The controls: binding=reversed's picture is outside
/// the rule, the stitch's own picture isn't a weights picture, and M3.5's
/// refusal ("the GPU's stitch arrives in M3.7") isn't the refusal.
#[test]
fn frame_insv_stitches_on_the_gpu() {
    let _t = timed("frame_insv_stitches_on_the_gpu");
    let dir = outputs("gpu");
    let reversed: Vec<&str> = STITCHED
        .iter()
        .map(|a| match *a {
            "binding=direct" => "binding=reversed",
            a => a,
        })
        .collect();
    let weights = [
        "--projection",
        "equirect",
        "--size",
        "512x256",
        "--seam",
        "weights",
    ];
    let small: [(&str, &[&str]); 4] = [
        ("stitched", &STITCHED),
        ("reversed", &reversed),
        ("lens0", &LENS0),
        ("weights", &weights),
    ];
    type Other<'a> = (&'a str, fn() -> &'static Path, &'a [&'a str]);
    let others: [Other; 2] = [
        (
            "no-calibration",
            || pair(Some(InsvFault::NoCalibration)),
            &["--size", "64x32"],
        ),
        ("equirect", pair_file, &["--size", "64x32", "--equirect"]),
    ];
    let (reference, lens0, ran, other_runs) = std::thread::scope(|s| {
        let reference = s.spawn(reference_equirect);
        let lens0 = s.spawn(reference_lens0);
        let other_runs = s.spawn(|| {
            each(&others, |(name, file, args)| {
                let out = dir.join(format!("{name}.png"));
                (frame(file(), &out, args), out)
            })
        });
        let ran = each(&small, |(name, args)| {
            let out = dir.join(format!("{name}.png"));
            (frame(small_file(), &out, args), out)
        });
        (
            reference.join().unwrap(),
            lens0.join().unwrap(),
            ran,
            other_runs.join().unwrap(),
        )
    });
    let [
        (stitched, stitched_out),
        (reversed, reversed_out),
        (lens, lens_out),
        (weights, weights_out),
    ] = &ran[..]
    else {
        unreachable!()
    };
    let r = stitched.ok();
    assert_eq!(r["renderer"], "gpu", "{r}");
    assert_eq!(r["gpu"]["device_type"], "cpu", "{r}");
    assert_eq!(r["view"]["projection"], "equirect", "{r}");
    assert_eq!(r["stitch"]["geometry"]["status"], "user", "{r}");
    assert_eq!(r["stitch"]["distance_m"], Value::Null, "{r}");
    let want = reference.0.ok();
    assert_eq!(want["renderer"], "reference", "{want}");
    let pts: Vec<&Value> = r["stitch"]["lenses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| &l["pts"])
        .collect();
    assert_eq!(pts.len(), 2, "{r}");
    assert_eq!(pts[0], pts[1], "{r}");
    assert_eq!(pts[0], &r["frame"]["pts"], "{r}");
    assert_eq!(r["frame"]["pts"], want["frame"]["pts"], "{r}");
    assert_eq!(r["frame"]["time"]["frame"], 5, "{r}");
    let (gpu, cpu) = (load(stitched_out), load(&reference.1));
    within_cpu_rule(&gpu, &cpu).unwrap();
    // The control: binding=reversed, on the GPU.
    assert_eq!(reversed.ok()["renderer"], "gpu");
    must_fail(
        "binding=reversed against binding=direct's reference",
        within_cpu_rule(&load(reversed_out), &cpu),
    );
    // --lens 0, flat on the GPU.
    let r = lens.ok();
    assert_eq!(r["renderer"], "gpu", "{r}");
    assert_eq!(r["source"]["stream"], 0, "{r}");
    assert_eq!(r["stitch"], Value::Null, "{r}");
    assert_eq!(lens0.0.ok()["renderer"], "reference");
    within_cpu_rule(&load(lens_out), &load(&lens0.1)).unwrap();
    // --seam weights, and the control: the stitch's own picture.
    assert_eq!(weights.ok()["renderer"], "gpu");
    weights_sum_to_one(&load(weights_out)).unwrap();
    must_fail("the stitch's picture as weights", weights_sum_to_one(&gpu));
    // No calibration: the refusal, and M3.5's for the control.
    let [(uncalibrated, uncalibrated_out), (equirect, _)] = &other_runs[..] else {
        unreachable!()
    };
    refused_for_its_calibration(uncalibrated, uncalibrated_out).unwrap();
    let mut doc = uncalibrated.doc.clone();
    doc["error"]["message"] = format!(
        "{}; --reference stitches it on the CPU, and the GPU's stitch arrives in M3.7",
        uncalibrated.message()
    )
    .into();
    let m3_5_words = Ran {
        status: uncalibrated.status,
        doc,
        stderr: String::new(),
    };
    must_fail(
        "M3.5's words",
        refused_for_its_calibration(&m3_5_words, uncalibrated_out),
    );
    // --equirect: the first stream as an equirect, on the GPU, no stitch.
    let r = equirect.ok();
    assert_eq!(r["renderer"], "gpu", "{r}");
    assert_eq!(r["source"]["stream"], 0, "{r}");
    assert_eq!(r["stitch"], Value::Null, "{r}");
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
    let (lens_cases, mut lenses, small0, seamed) = std::thread::scope(|s| {
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
                        "512x256",
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
        // small()'s lens 0 is reference_lens0's run.
        let small0 = s.spawn(reference_lens0);
        let lenses = each(&lens_cases[1..], |(file, name, n)| {
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
        (
            lens_cases,
            lenses,
            small0.join().unwrap(),
            seamed.join().unwrap(),
        )
    });
    let small0 = (
        Ran {
            status: small0.0.status,
            doc: small0.0.doc.clone(),
            stderr: small0.0.stderr.clone(),
        },
        small0.1.clone(),
    );
    lenses.insert(0, small0);
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

/// Whether a run rendered with one `code` warning, whose message holds
/// `what`.
fn warned_once(ran: &Ran, code: &str, what: &str) -> Result<(), String> {
    if ran.status != Some(0) || ran.doc["ok"] != true {
        return Err(format!("it failed: {} / {}", ran.doc, ran.stderr));
    }
    let of_code: Vec<&Value> = ran.doc["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == code)
        .collect();
    match &of_code[..] {
        [w] if w["message"].as_str().is_some_and(|m| m.contains(what)) => Ok(()),
        _ => Err(format!(
            "not one {code} warning with {what:?}: {}",
            ran.doc["warnings"]
        )),
    }
}

/// Probe's damage is a warning on a camera's pair as on any file,
/// whether the pair is stitched (`--reference`) or a lens is shown
/// (`--lens 0`): a master whose AAC track is damaged, its trailer
/// intact, renders with one `damaged` warning naming that track. A pair
/// whose model has no entry in the table is stitched by the X5's, with
/// one `geometry_assumed` warning giving the table's notice, which
/// `stitch.geometry.notice` gives too. The controls: the same master
/// without the damage, the X5's, gives no `damaged` warning, stitched
/// or as a lens, and stitched it gives no `geometry_assumed` warning
/// and its notice is null. A lens alone is never stitched, so it has no
/// geometry to assume.
#[test]
fn frame_insv_warns_of_damage_and_an_assumed_geometry() {
    /// A run: its name, the file it renders and its flags.
    type Case = (&'static str, fn() -> &'static Path, &'static [&'static str]);
    let _t = timed("frame_insv_warns_of_damage_and_an_assumed_geometry");
    let dir = outputs("warns");
    let stitched: &'static [&'static str] = &["--reference", "--size", "64x32"];
    let lens: &'static [&'static str] = &["--reference", "--lens", "0", "--size", "64x64"];
    let cases: [Case; 5] = [
        ("damaged", damaged_pair, stitched),
        ("damaged-lens", damaged_pair, lens),
        ("unlisted", unlisted_pair, stitched),
        ("clean", pair_file, stitched),
        ("clean-lens", pair_file, lens),
    ];
    let ran = each(&cases, |(name, file, args)| {
        frame(file(), &dir.join(format!("{name}.png")), args)
    });
    let [damaged, damaged_lens, unlisted, clean, clean_lens] = &ran[..] else {
        unreachable!()
    };
    let damage = "track #2: its `mdhd` gives timescale 0";
    let notice = "geometry assumed from the X5";
    for (name, ran) in [("--reference", damaged), ("--lens 0", damaged_lens)] {
        warned_once(ran, "damaged", damage).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    warned_once(unlisted, "geometry_assumed", notice).unwrap();
    assert_eq!(unlisted.ok()["stitch"]["geometry"]["notice"], notice);
    assert_eq!(clean.ok()["stitch"]["geometry"]["notice"], Value::Null);
    // The controls: the master without the damage, the X5's.
    for (name, ran) in [("--reference", clean), ("--lens 0", clean_lens)] {
        must_fail(
            &format!("{name}'s damage, undamaged"),
            warned_once(ran, "damaged", damage),
        );
    }
    must_fail(
        "the X5's geometry, assumed",
        warned_once(clean, "geometry_assumed", notice),
    );
}

/// `--geometry` takes each of `Geometry`'s fields by its enum's names,
/// and `--stitch-distance` metres or `inf`; a bad key or value is a usage
/// error naming it in Spherecut's words, which clap's echo of the value
/// doesn't hold (the control: each one's words against the next one's
/// message); and each of `--lens`, `--seam`, `--geometry` and
/// `--stitch-distance` is a usage error with `--project`, `--equirect`
/// or `--stream`, or on a file that isn't a camera's pair, as `--lens`
/// is with any of the other three. A pair decodes as any file does: a
/// hardware decoder that isn't there (VA-API on a render node that
/// isn't, Vulkan Video with the loader shown lavapipe alone) is
/// `hw_unavailable`, exit 3, on the GPU and under `--reference` alike.
/// The controls: binding=direct and binding=reversed render different
/// pictures, and `--decoder software` on the pair decodes.
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
    // Each bad one, and the words its message must hold: Spherecut's,
    // naming the key or value, which clap's message, quoting the value
    // whole, doesn't hold unless they are there.
    let bad: [(&[&str], &[&str]); 8] = [
        (
            &["--geometry", "binding=sideways"],
            &["binding=sideways: binding is one of direct, reversed"],
        ),
        (
            &["--geometry", "colour=red"],
            &["no key colour: the keys are"],
        ),
        (&["--geometry", "binding"], &["binding: expected key=value"]),
        (
            &["--geometry", "canvas=d,binding=direct"],
            &["canvas=d: canvas is one of a, b, c"],
        ),
        (&["--geometry", "turn=y,turn=x"], &["turn is given twice"]),
        (
            &["--stitch-distance", "2"],
            &["expected metres such as 2m, or inf, not 2"],
        ),
        (
            &["--stitch-distance", "0.1m"],
            &["0.1m is nearer than the stitch reaches: give 0.3m or more"],
        ),
        (
            &["--stitch-distance", "-2m"],
            &["-2m is nearer than the stitch reaches"],
        ),
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
    // The decoders that aren't there, as on a one-stream file
    // (frame.rs's rows): the loader shown lavapipe alone has no Vulkan
    // Video, and a render node that isn't has no VA-API.
    let icd = lavapipe::lavapipe_icd();
    let only_lavapipe = [
        ("VK_DRIVER_FILES", icd.as_os_str()),
        ("VK_ICD_FILENAMES", icd.as_os_str()),
    ];
    let vaapi = ["--decoder", "vaapi", "--render-node", "/nonexistent"];
    type Env<'a> = &'a [(&'a str, &'a std::ffi::OsStr)];
    let decoders: [(Vec<&str>, Env); 3] = [
        (
            [&["--reference", "--size", "64x32"][..], &vaapi].concat(),
            &[],
        ),
        (
            vec!["--reference", "--size", "64x32", "--decoder", "vulkan"],
            &only_lavapipe,
        ),
        ([&["--size", "64x32"][..], &vaapi].concat(), &[]),
    ];
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
    let (parsed_runs, bad_runs, conflict_runs, project_runs, plain_runs, decoder_runs, pictures) =
        std::thread::scope(|s| {
            let parsed_runs = s.spawn(|| each(&parsed, |args| frame(&missing, &out, args)));
            let decoder_runs = s.spawn(|| {
                each(&decoders, |(args, env)| {
                    frame_in(pair_file(), &out, args, env)
                })
            });
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
                decoder_runs.join().unwrap(),
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
    for (k, ((args, names), ran)) in bad.iter().zip(&bad_runs).enumerate() {
        assert_eq!(ran.status, Some(2), "{args:?}: {}", ran.doc);
        assert_eq!(ran.code(), "usage", "{args:?}: {}", ran.doc);
        // Words the command line holds would be in clap's echo of it.
        for name in *names {
            assert!(!args.join(" ").contains(name), "{args:?}: {name:?}");
        }
        names_in(ran.message(), names).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        // The control: the next bad value's message.
        let next = &bad_runs[(k + 1) % bad.len()];
        must_fail(
            &format!("{args:?}'s words in the next bad value's message"),
            names_in(next.message(), names),
        );
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
    for ((args, _), ran) in decoders.iter().zip(&decoder_runs) {
        misses_decoder(ran).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    }
    let [(direct, direct_out), (reversed, reversed_out)] = &pictures[..] else {
        unreachable!()
    };
    // The control: the same pair under --reference, decoded in software.
    must_fail("--decoder software", misses_decoder(direct));
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

/// Whether a run failed for want of the decoder asked for, as on a
/// one-stream file: `hw_unavailable`, exit 3, with no picture.
fn misses_decoder(ran: &Ran) -> Result<(), String> {
    if ran.status == Some(frame_exit("hw_unavailable") as i32)
        && ran.code() == "hw_unavailable"
        && ran.doc["ok"] == false
    {
        Ok(())
    } else {
        Err(format!("exit {:?}: {}", ran.status, ran.doc))
    }
}

// A clear error, check by check, each with a control of its own.

/// Whether a run was refused: no picture, and `ok` false.
fn refused(ran: &Ran) -> Result<(), String> {
    if ran.status == Some(0) || ran.doc["ok"] != false {
        return Err(format!("it rendered: {}", ran.doc));
    }
    Ok(())
}

/// Whether a run exits as the exit table says for its code, which isn't
/// `internal`, Spherecut's own failure.
fn exits_by_table(ran: &Ran) -> Result<(), String> {
    if ran.status != Some(frame_exit(ran.code()) as i32) || ran.code() == "internal" {
        return Err(format!("exit {:?} for {}", ran.status, ran.doc));
    }
    Ok(())
}

/// Whether a run's stderr shows no panic.
fn no_panic(ran: &Ran) -> Result<(), String> {
    if ran.stderr.contains("panicked") {
        return Err(format!("a panic: {}", ran.stderr));
    }
    Ok(())
}

/// Whether nothing was written to `out` or beside it.
fn wrote_nothing(out: &Path) -> Result<(), String> {
    if out.exists() || !temp_files(out.parent().unwrap()).is_empty() {
        return Err("something was written".into());
    }
    Ok(())
}

/// Whether `message` holds each of `names`.
fn names_in(message: &str, names: &[&str]) -> Result<(), String> {
    for name in names {
        if !message.contains(name) {
            return Err(format!("no {name:?} in {message:?}"));
        }
    }
    Ok(())
}

/// Whether a run is a clear error for a trailer at fault: refused, by
/// the exit table, with no panic, nothing written, and `names` in its
/// message, which names `file` first, so that the names are looked for
/// in the rest.
fn clear_error(ran: &Ran, file: &Path, out: &Path, names: &[&str]) -> Result<(), String> {
    refused(ran)?;
    exits_by_table(ran)?;
    no_panic(ran)?;
    wrote_nothing(out)?;
    names_in(&said(ran, file), names)
}

/// A run's error message without the path of `file`, which it names.
fn said(ran: &Ran, file: &Path) -> String {
    ran.message()
        .replace(file.to_string_lossy().as_ref(), "FILE")
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
/// `bad_trailer`, and `frame --reference` is a clear error ([`clear_error`]):
/// refused, by the exit table, with no panic, nothing written, and a
/// message naming the record or field at fault. Under `BadMagic` probe
/// doesn't call the file Insta360's, so frame never reads the trailer,
/// and its refusal carries probe's warning; under `NoCalibration` it
/// says "no calibration".
///
/// Each check has a control that reaches it and fails it: the file
/// without a fault, which renders, for `refused`, for `wrote_nothing`
/// (its picture is there) and for the probe's and BadMagic's warnings;
/// a fault's own refusal said to exit 2, where the table gives 1, and
/// one said to be `internal`, for `exits_by_table`; a run that panics
/// (`SPHERECUT_DEBUG_PANIC=main`) for `no_panic`, in a debug build only,
/// since a release build has no such switch and runs `no_panic` without
/// its control; and for `names_in`, each fault's names against a sibling
/// fault's message, which names another part.
#[test]
fn frame_insv_trailer_faults_are_clear_errors() {
    let _t = timed("frame_insv_trailer_faults_are_clear_errors");
    let dir = outputs("faults");
    // Each fault, the words that name what is at fault, and the sibling
    // whose message lacks them.
    let faults: [(InsvFault, &[&str], usize); 4] = [
        (InsvFault::TruncatedTrailer, &["tail"], 3),
        (InsvFault::IndexSlotPastEnd, &["record 1"], 2),
        (InsvFault::WrongTokenCount, &["calibration", "tokens"], 1),
        (InsvFault::VarintOverflow, &["record 1", "varint"], 0),
    ];
    let all: Vec<Option<InsvFault>> = faults
        .iter()
        .map(|(f, ..)| Some(*f))
        .chain([
            Some(InsvFault::BadMagic),
            Some(InsvFault::NoCalibration),
            None,
        ])
        .collect();
    let files = each(&all, |f| pair(*f));
    let (ran, panicked) = std::thread::scope(|s| {
        // The run that panics, of the file without a fault: a debug
        // build's, since a release build has no such switch.
        let panicked = cfg!(debug_assertions).then(|| {
            s.spawn(|| {
                let out = dir.join("panicked.png");
                let o = spherecut()
                    .env("SPHERECUT_DEBUG_PANIC", "main")
                    .arg("frame")
                    .arg(files[files.len() - 1])
                    .arg("-o")
                    .arg(&out)
                    .args(["--json", "--reference", "--size", "64x32"])
                    .output()
                    .unwrap();
                Ran {
                    status: o.status.code(),
                    doc: document(&o.stdout),
                    stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
                }
            })
        });
        let ran = each(&files, |file| {
            let name = file.parent().unwrap().file_name().unwrap().to_owned();
            let out = dir.join(&name).join("out.png");
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            (frame(file, &out, &["--reference", "--size", "64x32"]), out)
        });
        (ran, panicked.map(|h| h.join().unwrap()))
    });
    let reports = probe(&files);
    let unfaulted = all.len() - 1;
    for (k, (fault, names, sibling)) in faults.iter().enumerate() {
        warns_bad_trailer(&reports[k]).unwrap_or_else(|e| panic!("{fault:?}: {e}"));
        let (r, out) = &ran[k];
        clear_error(r, files[k], out, names).unwrap_or_else(|e| panic!("{fault:?}: {e}"));
        assert_eq!(r.code(), "not_equirect", "{fault:?}: {}", r.doc);
        eprintln!("{fault:?}: {}", r.message());
        // The control for the names: the sibling's message.
        must_fail(
            &format!("{fault:?}'s names in {:?}'s message", faults[*sibling].0),
            names_in(&said(&ran[*sibling].0, files[*sibling]), names),
        );
    }
    // The controls for the rest.
    let (clean, clean_out) = &ran[unfaulted];
    must_fail(
        "the probe check, unfaulted",
        warns_bad_trailer(&reports[unfaulted]),
    );
    must_fail("refused, unfaulted", refused(clean));
    must_fail("nothing written, unfaulted", wrote_nothing(clean_out));
    let faulted = &ran[0].0;
    let said_so = |status: i32, code: &str| {
        let mut doc = faulted.doc.clone();
        doc["error"]["code"] = code.into();
        Ran {
            status: Some(status),
            doc,
            stderr: faulted.stderr.clone(),
        }
    };
    must_fail(
        "the exit table, a refusal that exits 2",
        exits_by_table(&said_so(2, faulted.code())),
    );
    must_fail(
        "the exit table, internal",
        exits_by_table(&said_so(1, "internal")),
    );
    if let Some(panicked) = &panicked {
        assert_eq!(panicked.code(), "internal", "{}", panicked.doc);
        must_fail("no panic, of a run that panicked", no_panic(panicked));
    }
    // BadMagic: not Insta360's to probe, which warns; frame's refusal
    // carries the warning.
    let k = faults.len();
    warns_bad_trailer(&reports[k]).unwrap();
    assert_ne!(reports[k]["camera"]["layout"], "insta360_dual_fisheye");
    let (r, out) = &ran[k];
    clear_error(r, files[k], out, &[]).unwrap();
    assert_eq!(r.code(), "not_equirect", "{}", r.doc);
    assert!(r.warnings().contains(&"bad_trailer"), "{}", r.doc);
    eprintln!("BadMagic: {}", r.message());
    must_fail(
        "BadMagic's warning, unfaulted",
        if clean.warnings().contains(&"bad_trailer") {
            Ok(())
        } else {
            Err(())
        },
    );
    // NoCalibration, and its names in TruncatedTrailer's message.
    let (r, out) = &ran[k + 1];
    clear_error(r, files[k + 1], out, &["no calibration"]).unwrap();
    eprintln!("NoCalibration: {}", r.message());
    must_fail(
        "NoCalibration's names in TruncatedTrailer's message",
        names_in(&said(&ran[0].0, files[0]), &["no calibration"]),
    );
}

/// `--projection equirect`, which M3.5 adds for `.insv` files, on a file
/// that is already a stitched equirect: the whole sphere through the
/// keyed turn, as `reference::render` draws it through
/// `ViewParams::equirect`; with `--fov` or `--morph`, and `--morph-to
/// equirect` on any projection, a usage error. Its run on lavapipe is
/// `frame.rs`'s `the_whole_sphere_on_lavapipe_keeps_to_the_cpu_rule`,
/// which this suite's budget had no room for.
#[test]
fn frame_projection_equirect_on_an_equirect_file() {
    let _t = timed("frame_projection_equirect_on_an_equirect_file");
    let file = equirect_file();
    let dir = outputs("projection-equirect");
    let size = (256, 128);
    let runs: [(&str, &[&str]); 6] = [
        ("yaw30", &["--reference", "--yaw", "30"]),
        ("yaw0", &["--reference"]),
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
    // The usage errors.
    for ((name, _), (ran, _)) in runs.iter().zip(&ran).skip(2) {
        assert_eq!(ran.status, Some(2), "{name}: {}", ran.doc);
        assert_eq!(ran.code(), "usage", "{name}: {}", ran.doc);
    }
}
