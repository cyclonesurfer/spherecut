//! Rung 3 of the round-trip ladder (M3 plan 4.2; M3.5's unit (c), M3.7's
//! unit (d)): the lossless synthetic `.insv` (`InsvSpec::rung3()`)
//! through probe, the camera crate and `Still::open_pair` to the CPU
//! reference and to the GPU on lavapipe, held to the planes synth coded,
//! to rung 1's render of them and to rung 2's (the GPU's stitch of
//! synth's planes, byte for byte), and `StillPair`'s seek and refusals,
//! and `pair_stitching`'s guard against the UI thread.
//!
//! Rung 1's picture is made here from `synth::rung`'s pair, synth's
//! planes with no file between, and the boundary crate's `sphere_rig`:
//! the camera, tune, frames and views render's `tests/instrument.rs`
//! (rung 1) takes from `synth::rung` too, so they are rung 1's own. Each
//! file is written once a binary, into a folder of this binary's kept
//! for the run, which a later run removes once this one has ended (as
//! cli's `frame_insv.rs` does), and synth renders each lens's picture
//! once a binary through `RenderCache::shared()`, for `write_insv` and
//! `synth::rung` alike. A test writes the files its controls need, and
//! runs its controls, beside its own check rather than after it: rung
//! 3's file alone takes most of the budget to write.
//!
//! Budget (4.1 rule 5, 19.6): 2.3 s, the binary's measured time on the
//! cloud host with the GPU half (2.05 to 2.11 s without it), within the
//! 3.1 s M3.7 allows it, rung 1's and rung 2's own renders of their pair
//! included. Each test prints its time and the suite's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use spherecut_agent::still::{
    self, DecodedPair, Decoder, Failure, Look, Options, PairOptions, Still, StillPair, Stop,
};
use spherecut_boundary::sphere_rig;
use spherecut_core::TimeSpec;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with};
use spherecut_core::cancel::StopSource;
use spherecut_core::video::VideoPlanes;
use spherecut_media::Source;
use spherecut_media::backend::SeekMode;
use spherecut_render::reference::{self, RigOptions};
use spherecut_render::{
    AdapterChoice, Depth, Image, LensFrame, PlaneSource, SourceFrames, StillOptions, StillRenderer,
    ViewParams,
};
use spherecut_sphere::rig::{Binding, Generation, Geometry, Rig, Tune};
use spherecut_synth::Pattern;
use spherecut_synth::code::{CornerCode, Luma, read_corner_code};
use spherecut_synth::insv::{InsvFault, InsvSpec, Quality, write_insv};
use spherecut_synth::media::{pairs_64, write_pair};
use spherecut_synth::rung::{self, Shot};
use spherecut_synth::yuv::YuvPlanes;
use spherecut_testkit::control::must_fail;
use spherecut_testkit::{Fnv64, fnv64};

/// The suite's budget in seconds (amendment 5, 19.6).
const BUDGET_S: f64 = 2.3;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the suite's budget. The tests run
/// side by side, so none of them can time the rest: the last line
/// printed is the suite's time.
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

/// Unit gains, as `StillPair::render` stitches with.
const UNIT: [[f64; 3]; 2] = [[1.0; 3]; 2];

// The files.

/// The name of a run's folder, before the id of its process.
const RUN: &str = "rung3-";

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

/// `spec`'s master, written into a folder of its own.
fn written(spec: &InsvSpec, folder: &str) -> PathBuf {
    write_insv(spec, &tmp().join(folder)).unwrap().master
}

/// Rung 3's file: 8 lossless frames of 2 × 768².
fn rung3_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| written(&InsvSpec::rung3(), "rung3"))
}

/// The file the other tests open: 30 lossless frames of 2 × 384².
fn small_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| written(&InsvSpec::small(), "small"))
}

/// `small()` with `fault` planted, 8 frames long: enough for frame 5,
/// and a quarter of `small()`'s coding.
fn faulty(fault: InsvFault, folder: &str) -> PathBuf {
    let spec = InsvSpec {
        fault: Some(fault),
        frames: 8,
        ..InsvSpec::small()
    };
    written(&spec, folder)
}

/// `small()` with lens 1 a frame late from the start
/// (`InsvFault::StartPtsDiffer`).
fn late_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| faulty(InsvFault::StartPtsDiffer, "start-pts"))
}

/// `small()` with no calibration (`InsvFault::NoCalibration`).
fn uncalibrated_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| faulty(InsvFault::NoCalibration, "no-calibration"))
}

/// `small()` with every calibration string a token short
/// (`InsvFault::WrongTokenCount`): a trailer that reads, with strings
/// that don't.
fn bad_strings_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| faulty(InsvFault::WrongTokenCount, "wrong-token-count"))
}

/// Rung 3's file coded as a camera codes, at crf 28, two frames long.
fn lossy_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let spec = InsvSpec {
            quality: Quality::CameraLike { crf: 28 },
            frames: 2,
            ..InsvSpec::rung3()
        };
        written(&spec, "rung3-crf28")
    })
}

/// Two lens tracks with no trailer: `write_pair`'s matched 64² pair.
fn plain_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| write_pair(&pairs_64()[0], &tmp().join("plain")).unwrap())
}

/// A stop nobody sets.
fn stop() -> Stop {
    Stop::of(&StopSource::new())
}

/// `path` probed and opened as a pair with no GPU, or the failure of
/// either; and the warnings.
fn open(path: &Path, pair: &PairOptions) -> (Result<StillPair<'static>, Failure>, Vec<String>) {
    open_on(path, None, pair)
}

/// [`open`] on `gpu`. Both lenses decode in software whatever the host
/// has: `open_pair` takes the caller's decoder since M3.7, and rung 3's
/// equalities are of the file's planes as FFmpeg's software decoder gives
/// them, never a hardware decoder's (the M3.7 review).
fn open_on<'g>(
    path: &Path,
    gpu: Option<&'g spherecut_render::Gpu>,
    pair: &PairOptions,
) -> (Result<StillPair<'g>, Failure>, Vec<String>) {
    let src = Source::path(path);
    let stop = stop();
    let mut warnings = Vec::new();
    let opened = still::probe_report(src, &stop).and_then(|report| {
        Still::open_pair(
            src,
            &report,
            gpu,
            &Options {
                decoder: Decoder::Software,
                ..Options::default()
            },
            pair,
            &stop,
            &mut warnings,
        )
    });
    let said = warnings.iter().map(|w| w.message.clone()).collect();
    (opened, said)
}

/// Both lenses at frame `n`, exactly.
fn seek(pair: &mut StillPair<'_>, n: u64) -> Result<DecodedPair, Failure> {
    pair.seek(TimeSpec::Frame(n), SeekMode::Exact, &mut Vec::new())
}

// Hashes.

/// FNV-1a of each of a decoded picture's planes, row by row without the
/// rows' padding.
fn decoded_hashes(picture: &dyn VideoPlanes) -> Vec<u64> {
    (0..picture.layout().planes.len())
        .map(|i| {
            let plane = picture.plane(i).unwrap();
            let mut h = Fnv64::new();
            for y in 0..plane.height {
                h.write(plane.row(y, plane.width as usize).unwrap());
            }
            h.finish()
        })
        .collect()
}

/// FNV-1a of each of synth's 8-bit planes, which hold their rows without
/// padding.
fn planes_hashes(planes: &YuvPlanes) -> Vec<u64> {
    let YuvPlanes::U8(p) = planes else {
        panic!("not 8-bit");
    };
    p.iter().map(|plane| fnv64(plane)).collect()
}

/// Whether two sets of hashes are the same, saying which plane differs.
fn same_hashes(got: &[u64], want: &[u64]) -> Result<(), String> {
    match (0..got.len().max(want.len())).find(|&i| got.get(i) != want.get(i)) {
        Some(i) => Err(format!("plane {i}")),
        None => Ok(()),
    }
}

// Pictures.

/// Rung 1's view as render's type.
fn view_of(shot: &Shot) -> ViewParams {
    match shot.lens {
        None => ViewParams::equirect(shot.turn),
        Some(lens) => ViewParams::new(shot.turn, lens),
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
    let (spherecut_render::Pixels::Rgb8(a), spherecut_render::Pixels::Rgb8(b)) =
        (&got.pixels, &want.pixels)
    else {
        return Err("not both 8-bit".into());
    };
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

/// The geometry `rung3()` planted: the entry stitching gives the file
/// (its generation field 136's, V3), read by the binding
/// `x5_like_planted` has, Direct, where the X5's row has Reversed.
fn planted_geometry(path: &Path) -> Geometry {
    let entry = still::pair_stitching(Source::path(path)).unwrap();
    assert_eq!(entry.known.geometry.generation, Generation::V3);
    assert_eq!(entry.known.geometry.binding, Binding::Reversed);
    Geometry {
        binding: Binding::Direct,
        ..entry.known.geometry
    }
}

/// The decoded planes of each of rung 3's 8 frames, on both tracks, hash
/// equal to `InsvSpec::planes` for that track and frame: the decode is
/// lossless.
#[test]
fn rung3_planes_hash_equal() {
    let _t = timed("rung3_planes_hash_equal");
    let spec = InsvSpec::rung3();
    std::thread::scope(|s| {
        let lossy = s.spawn(lossy_file);
        // synth's planes, a thread a track, beside the decode.
        let synth = [0, 1].map(|t| {
            let spec = &spec;
            s.spawn(move || {
                (0..spec.frames)
                    .map(|k| planes_hashes(&spec.planes(t, k).unwrap()))
                    .collect::<Vec<_>>()
            })
        });
        let (opened, _) = open(rung3_file(), &PairOptions::default());
        let mut pair = opened.unwrap();
        let mut got = Vec::new();
        for k in 0..spec.frames {
            let decoded = seek(&mut pair, u64::from(k)).unwrap();
            got.push(
                decoded
                    .frames
                    .each_ref()
                    .map(|f| decoded_hashes(f.as_ref())),
            );
        }
        let [synth0, synth1] = synth.map(|h| h.join().unwrap());
        let want: Vec<[Vec<u64>; 2]> = synth0
            .into_iter()
            .zip(synth1)
            .map(|(a, b)| [a, b])
            .collect();
        held_to_planes(&got, &want, lossy.join().unwrap());
    });
}

/// `got`'s hashes, by frame and track, are `want`'s; and the controls:
/// each frame against the next's planes, track 0 against track 1's, and
/// the first frame of `lossy`, rung 3's file coded at crf 28, against
/// frame 0's.
fn held_to_planes(got: &[[Vec<u64>; 2]], want: &[[Vec<u64>; 2]], lossy: &Path) {
    for (k, (got, want)) in got.iter().zip(want).enumerate() {
        for t in 0..2 {
            same_hashes(&got[t], &want[t]).unwrap_or_else(|e| panic!("frame {k}, track {t}: {e}"));
        }
    }
    for k in 0..got.len() {
        for t in 0..2 {
            if k + 1 < got.len() {
                must_fail(
                    &format!("frame {k} of track {t} against frame {}", k + 1),
                    same_hashes(&got[k][t], &want[k + 1][t]),
                );
            }
        }
        must_fail(
            &format!("frame {k} of track 0 against track 1's"),
            same_hashes(&got[k][0], &want[k][1]),
        );
    }
    let (opened, _) = open(lossy, &PairOptions::default());
    let mut twin = opened.unwrap();
    let decoded = seek(&mut twin, 0).unwrap();
    for (t, (frame, want)) in decoded.frames.iter().zip(&want[0]).enumerate() {
        must_fail(
            &format!("track {t} of the crf 28 twin"),
            same_hashes(&decoded_hashes(frame.as_ref()), want),
        );
    }
}

/// `StillPair::render` of the file equals rung 1's `render_rig` of the
/// same planes and rig, the one through the file and the camera crate,
/// the other through synth and the boundary.
#[test]
fn rung3_render_equals_rung1() {
    let _t = timed("rung3_render_equals_rung1");
    let shots = [rung::equirect(), rung::whole_view(0)];
    // Rung 1's: synth's planes, no file, through Rig::build of the
    // boundary's Calibration and Geometry, stitched infinitely far.
    let camera = rung::camera();
    let rig = |tune: &Tune| -> Rig {
        sphere_rig(
            &camera.cal,
            &camera.planted,
            tune,
            rung::FRONT_TRACK,
            rung::SIDES,
        )
        .unwrap()
    };
    let rung1_rig = rig(&rung::far());
    let frames = rung::pair(Pattern::SeamLines);
    let ready = reference::LensPair::new([&frames[0], &frames[1]]).unwrap();
    let rung1: Vec<Image> = shots
        .iter()
        .map(|shot| {
            let picture = reference::render_rig_of(
                &ready,
                &rung1_rig,
                UNIT,
                &view_of(shot),
                shot.size,
                &RigOptions::default(),
            )
            .unwrap();
            still::quantize(&picture, Depth::Eight)
        })
        .collect();

    // The file's, under the geometry it planted and the same tune.
    let planted = planted_geometry(rung3_file());
    let options = |geometry: Option<Geometry>, tune: Tune| PairOptions {
        geometry,
        tune,
        rig: RigOptions::default(),
    };
    let render = |pair: &mut StillPair<'_>, decoded: &DecodedPair, shot: &Shot| {
        pair.render(decoded, &[view_of(shot)], shot.size, Look::default())
            .unwrap()
    };
    let rung1 = &rung1;
    std::thread::scope(|s| {
        // Control: the entry stitching gives, unchanged (Reversed),
        // builds and renders, and isn't rung 1's picture.
        let as_stored = s.spawn(|| {
            let (opened, _) = open(rung3_file(), &options(None, rung::far()));
            let mut as_stored = opened.unwrap();
            assert_eq!(as_stored.geometry().binding, Binding::Reversed);
            let decoded = seek(&mut as_stored, 0).unwrap();
            must_fail(
                "the X5 row's binding, Reversed",
                same_picture(&render(&mut as_stored, &decoded, &shots[0]), &rung1[0]),
            );
        });
        // Control: the default tune, stitched at 2 m.
        let at_2m = s.spawn(|| {
            let (opened, _) = open(
                rung3_file(),
                &options(Some(planted.clone()), Tune::default()),
            );
            let mut at_2m = opened.unwrap();
            must_fail("the default tune's rig", at_2m.rig() == &rung1_rig);
            let decoded = seek(&mut at_2m, 0).unwrap();
            must_fail(
                "the default tune's picture",
                same_picture(&render(&mut at_2m, &decoded, &shots[0]), &rung1[0]),
            );
        });
        let (opened, _) = open(rung3_file(), &options(Some(planted.clone()), rung::far()));
        let mut pair = opened.unwrap();
        assert_eq!(pair.rig(), &rung1_rig);
        let decoded = seek(&mut pair, 0).unwrap();
        // The decoded frames carry the colour synth's frames do, as the
        // reference reads it.
        let colour = |p: &dyn VideoPlanes| {
            let c = p.color();
            (c.range, c.matrix, c.transfer, c.siting)
        };
        for (t, frame) in decoded.frames.iter().enumerate() {
            assert_eq!(colour(frame.as_ref()), colour(&frames[t]), "track {t}");
        }
        for (shot, want) in shots.iter().zip(rung1) {
            same_picture(&render(&mut pair, &decoded, shot), want)
                .unwrap_or_else(|e| panic!("{shot:?}: {e}"));
        }
        as_stored.join().unwrap();
        at_2m.join().unwrap();
    });
}

/// Rung 3's GPU half: `StillPair::render` of the file on lavapipe equals
/// rung 2's stitch of the same planes and rig, byte for byte: the one
/// through the file, the camera crate and `Still::open_pair` with the
/// GPU, the other `render_blurred` of synth's planes (a
/// `SourceFrames::Pair` of their bytes) and the boundary's rig, as
/// render's `tests/dfish/rung2.rs` draws them. Both through the same
/// views at the same depth, with unit gains. The control opens the file
/// under the entry stitching gives it, unchanged (Reversed), which
/// renders and isn't rung 2's picture.
#[test]
fn rung3_gpu_render_equals_rung2() {
    let _t = timed("rung3_gpu_render_equals_rung2");
    let shots = [rung::equirect(), rung::whole_view(0)];
    let gpu = std::thread::spawn(|| still::gpu(&AdapterChoice::Cpu, &Options::default()));
    let camera = rung::camera();
    let rig = sphere_rig(
        &camera.cal,
        &camera.planted,
        &rung::far(),
        rung::FRONT_TRACK,
        rung::SIDES,
    )
    .unwrap();
    let frames = rung::pair(Pattern::SeamLines);
    let planted = planted_geometry(rung3_file());
    let gpu = gpu.join().unwrap().unwrap();
    // Rung 2's: synth's planes, uploaded from their bytes.
    let mut renderer = StillRenderer::new(&gpu);
    let rung2: Vec<Image> = shots
        .iter()
        .map(|shot| {
            let lenses = frames.each_ref().map(|frame| LensFrame {
                frame,
                from: PlaneSource::Bytes,
            });
            let source = SourceFrames::Pair {
                lenses,
                rig: &rig,
                gains: UNIT,
            };
            renderer
                .render_blurred(
                    &gpu,
                    source,
                    &[view_of(shot)],
                    shot.size,
                    &StillOptions::default(),
                )
                .unwrap()
        })
        .collect();
    let options = |geometry: Option<Geometry>| PairOptions {
        geometry,
        tune: rung::far(),
        rig: RigOptions::default(),
    };
    let render = |pair: &mut StillPair<'_>, decoded: &DecodedPair, shot: &Shot| {
        pair.render(decoded, &[view_of(shot)], shot.size, Look::default())
            .unwrap()
    };
    let (gpu, rung2) = (&gpu, &rung2);
    std::thread::scope(|s| {
        // Control: the entry stitching gives, unchanged (Reversed).
        let as_stored = s.spawn(|| {
            let (opened, _) = open_on(rung3_file(), Some(gpu), &options(None));
            let mut as_stored = opened.unwrap();
            assert_eq!(as_stored.geometry().binding, Binding::Reversed);
            let decoded = seek(&mut as_stored, 0).unwrap();
            must_fail(
                "the X5 row's binding, Reversed, on the GPU",
                same_picture(&render(&mut as_stored, &decoded, &shots[0]), &rung2[0]),
            );
        });
        let (opened, _) = open_on(rung3_file(), Some(gpu), &options(Some(planted.clone())));
        let mut pair = opened.unwrap();
        assert_eq!(pair.rig(), &rig);
        let decoded = seek(&mut pair, 0).unwrap();
        for (shot, want) in shots.iter().zip(rung2) {
            same_picture(&render(&mut pair, &decoded, shot), want)
                .unwrap_or_else(|e| panic!("{shot:?}: {e}"));
        }
        as_stored.join().unwrap();
    });
}

/// `StillPair::seek` lands both lenses on the frame asked for, at one
/// time with equal pts, from any frame before or after it.
#[test]
fn open_pair_seeks_both_lenses_to_one_time() {
    let _t = timed("open_pair_seeks_both_lenses_to_one_time");
    let late = std::thread::spawn(late_file);
    let (opened, _) = open(small_file(), &PairOptions::default());
    let mut pair = opened.unwrap();
    // From the start, from after frame 5 and from before it.
    for from in [None, Some(20), Some(1)] {
        if let Some(from) = from {
            seek(&mut pair, from).unwrap();
        }
        let decoded = seek(&mut pair, 5).unwrap();
        assert_eq!(
            decoded.frames[0].pts, decoded.frames[1].pts,
            "from {from:?}"
        );
        assert_eq!(decoded.shown.frame, Some(5));
        for (t, frame) in decoded.frames.iter().enumerate() {
            let code = read_corner_code(&Luma::of_picture(frame.as_ref()).unwrap()).unwrap();
            assert_eq!(code, CornerCode::of(t, 5), "track {t}, from {from:?}");
        }
    }
    // Control: lens 1 a frame late from the start, so that its frame 5
    // is at pts 6006 where lens 0's is at 5005 (1/30000 s).
    let (opened, _) = open(late.join().unwrap(), &PairOptions::default());
    let mut late = opened.unwrap();
    let e = must_fail("lens 1 a frame late", seek(&mut late, 5));
    assert_eq!(e.code, "damaged", "{e:?}");
    for pts in ["5005", "6006"] {
        assert!(e.message.contains(pts), "{pts}: {}", e.message);
    }
}

/// `Still::open_pair` refuses a file it can't stitch, each with a clear
/// failure: one with no calibration (`InsvFault::NoCalibration`), one
/// whose calibration strings can't be read (`InsvFault::WrongTokenCount`),
/// a pair of tracks with no trailer, and a geometry `Rig::build` refuses;
/// and with no GPU `render` refuses more than one view.
#[test]
fn open_pair_refuses_a_file_it_cant_stitch() {
    let _t = timed("open_pair_refuses_a_file_it_cant_stitch");
    let refused = |(opened, _): (Result<StillPair<'_>, Failure>, Vec<String>)| match opened {
        Ok(p) => panic!("opened {p:?}"),
        Err(e) => e,
    };
    let files = [uncalibrated_file, bad_strings_file, plain_file].map(std::thread::spawn);
    small_file();
    let [uncalibrated, bad_strings, plain] = files.map(|f| f.join().unwrap());
    // No calibration.
    let e = refused(open(uncalibrated, &PairOptions::default()));
    assert_eq!(e.code, "not_equirect", "{e:?}");
    assert!(e.message.contains("no calibration"), "{}", e.message);
    // Strings that don't parse: not_equirect, never probe's bad_trailer,
    // which is a warning, with camera's words for the string at fault.
    let e = refused(open(bad_strings, &PairOptions::default()));
    assert_eq!(e.code, "not_equirect", "{e:?}");
    for words in ["can't be stitched", "calibration can't be read", "token"] {
        assert!(e.message.contains(words), "{words}: {}", e.message);
    }
    // Two lens tracks with no trailer: no camera's pair.
    let e = refused(open(plain, &PairOptions::default()));
    assert_eq!(e.code, "not_equirect", "{e:?}");
    assert!(e.message.contains("pair"), "{}", e.message);
    // V6 on a trailer of V3 alone: no rig.
    let entry = still::pair_stitching(Source::path(small_file())).unwrap();
    let v6 = Geometry {
        generation: Generation::V6,
        ..entry.known.geometry
    };
    let e = refused(open(
        small_file(),
        &PairOptions {
            geometry: Some(v6),
            ..PairOptions::default()
        },
    ));
    assert_eq!(e.code, "not_equirect", "{e:?}");
    assert!(e.message.contains("no rig"), "{}", e.message);
    // Two views, on the CPU reference.
    let (opened, warnings) = open(small_file(), &PairOptions::default());
    let mut pair = opened.unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    let decoded = seek(&mut pair, 0).unwrap();
    let shot = rung::equirect();
    let view = view_of(&shot);
    let e = pair
        .render(&decoded, &[view, view], (64, 32), Look::default())
        .unwrap_err();
    assert_eq!(e.code, "internal", "{e:?}");
    assert!(e.message.contains("one view"), "{}", e.message);
    // Control: the same file renders through one view.
    let image = pair
        .render(&decoded, &[view], (64, 32), Look::default())
        .unwrap();
    assert_eq!((image.width, image.height), (64, 32));
}

/// What `f` panicked with, run on a thread of its own, marked as the UI
/// thread when `marked`; `None` if it returned.
fn panics_on(marked: bool, f: impl FnOnce() + Send + 'static) -> Option<String> {
    std::thread::spawn(move || {
        if marked {
            mark_ui_thread_with(OnBlock::Panic);
        }
        f();
    })
    .join()
    .err()
    .map(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
}

/// `pair_stitching` calls `may_block` before it opens the file: on a
/// thread marked as the UI thread it panics, even for a path that
/// doesn't exist, which it would answer without reading a byte.
#[test]
fn pair_stitching_panics_on_the_ui_thread() {
    let _t = timed("pair_stitching_panics_on_the_ui_thread");
    let missing = tmp().join("missing.insv");
    let call = |marked| {
        let missing = missing.clone();
        panics_on(marked, move || {
            drop(still::pair_stitching(Source::path(&missing)));
        })
    };
    let m = call(true).expect("pair_stitching returned on the UI thread");
    assert!(
        m.starts_with("reading an Insta360 file's trailer would block the UI thread"),
        "{m}"
    );
    // The control: unmarked, the same call returns, media's failure for
    // a file that isn't there.
    must_fail(
        "pair_stitching on a thread not marked",
        call(false).is_some(),
    );
    let e = still::pair_stitching(Source::path(&missing)).unwrap_err();
    assert_eq!(e.code, "not_found", "{e:?}");
}
