//! A camera file's project media (M3 plan 11.4 and 11.5): media's
//! `camera_source` of synth's two-lens files, the superseded-geometry
//! check against the built-in table, and the export and viewer fixture,
//! a `set_source` and a `set_stitch` on the rung-3 file.
//!
//! Each file is written once a binary, into a folder of this binary's
//! kept for the run, which a later run removes once this one has ended
//! (as agent's `rung3.rs` does), and each write prints its time. The
//! fixture's rung-3 file is the one the first three tests read too, so
//! the suite codes one file: the others are it cut before its trailer,
//! or with another trailer appended.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::f64::consts::PI;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use glam::DVec3;
use spherecut_boundary::sphere_rig;
use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{
    Metadata, Stitching, known, parse_metadata, read_record, read_trailer, stitching,
};
use spherecut_core::project::{
    DualFisheye, Edit, Gains, INITIAL_VIEW, Media, Project, RawStrings, Source, gains_basis,
};
use spherecut_media::backend::{FfmpegBackend, MediaBackend, VideoRequest};
use spherecut_media::probe::{ProbeReport, probe};
use spherecut_media::project::{
    Superseded, camera_source, project_media, superseded, superseded_by_table,
};
use spherecut_sphere::rig::{Binding, Generation, Geometry, Rig, Status, Tune};
use spherecut_synth::insv::{InsvFault, InsvSpec, Records, Value, append_trailer};
use spherecut_synth::project::{PLANTED_BINDING, STITCH, StitchFixture, stitch_fixture};
use spherecut_synth::rung;
use spherecut_testkit::control::{at_least, at_most, must_fail};

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run. The tests run side by side, so none of
/// them can time the rest: the last line printed is the suite's time.
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
            "{}: {:.3} s; the suite {suite:.3} s so far",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

// The files.

/// The name of a run's folder, before the id of its process.
const RUN: &str = "synth-project-";

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

/// `make`'s result, printing how long it took to write `what`.
fn written<T>(what: &str, make: impl FnOnce() -> T) -> T {
    let began = Instant::now();
    let out = make();
    eprintln!("wrote {what} in {:.3} s", began.elapsed().as_secs_f64());
    out
}

/// 11.5's fixture, of rung 3's file: 8 lossless frames of 2 × 768².
fn fixture() -> &'static StitchFixture {
    static FIXTURE: OnceLock<StitchFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        written("the fixture (rung 3's file)", || {
            stitch_fixture(&tmp().join("fixture")).unwrap()
        })
    })
}

/// The fixture's file.
fn rung3_file() -> &'static Path {
    &fixture().files.master
}

/// The fixture's file cut before its trailer: its two lens tracks and
/// its audio, a plain MP4.
fn plain_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let f = fixture();
        written("plain", || {
            // The `inst` box starts 8 bytes, its header, before the
            // trailer's payload.
            let mp4 = f.files.layout.payload_start - 8;
            let bytes = std::fs::read(&f.files.master).unwrap();
            let dir = tmp().join("plain");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("plain.mp4");
            std::fs::write(&path, &bytes[..mp4 as usize]).unwrap();
            path
        })
    })
}

/// [`plain_file`] with `records` and `fault` appended as its trailer, in
/// `folder`, under the fixture's file's name: rung 3's file but for its
/// trailer, written with no coding.
fn retrailered(folder: &str, records: &Records, fault: Option<InsvFault>) -> PathBuf {
    let bare = plain_file();
    written(folder, || {
        let dir = tmp().join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(&fixture().spec.name);
        append_trailer(bare, &path, records, fault).unwrap();
        path
    })
}

/// Rung 3's file with no calibration (`InsvFault::NoCalibration`).
fn uncalibrated_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        retrailered(
            "no-calibration",
            &fixture().spec.records,
            Some(InsvFault::NoCalibration),
        )
    })
}

/// `s` with the digit after its last decimal point one more, or 8 for a
/// 9: a number of the string changed in its fraction, so the string
/// still parses.
fn a_digit_off(s: &str) -> String {
    let mut b = s.as_bytes().to_vec();
    let at = s.rfind('.').unwrap() + 1;
    assert!(b[at].is_ascii_digit(), "{s}");
    b[at] = if b[at] == b'9' { b'8' } else { b[at] + 1 };
    String::from_utf8(b).unwrap()
}

/// Rung 3's file with its V3 string, the one it holds and the camera
/// uses, a digit off: the working string (field 54) and its original
/// (56) alike.
fn digit_off_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let mut records = fixture().spec.records.clone();
        let mut changed = 0;
        for f in &mut records.metadata {
            if let (54 | 56, Value::Bytes(b)) = (f.number, &mut f.value) {
                *b = a_digit_off(std::str::from_utf8(b).unwrap()).into_bytes();
                changed += 1;
            }
        }
        assert_eq!(changed, 2);
        retrailered("digit-off", &records, None)
    })
}

/// Rung 3's file with field 27's offset, which a stitch doesn't read
/// (U13), at (32, 16) in place of the (0, 0) synth writes.
fn offset_file() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let mut records = fixture().spec.records.clone();
        let window = records
            .metadata
            .iter_mut()
            .find(|f| f.number == 27)
            .unwrap();
        let Value::Message(items) = &mut window.value else {
            panic!("{window:?}");
        };
        let mut changed = 0;
        for item in items {
            let to = match item.number {
                5 => 32,
                6 => 16,
                _ => continue,
            };
            item.value = Value::Varint(to);
            changed += 1;
        }
        assert_eq!(changed, 2);
        retrailered("offset", &records, None)
    })
}

// Reading them.

/// `path`'s source, as `camera_source` gives it after probe.
fn source_of(path: &Path) -> (Result<Option<DualFisheye>, String>, ProbeReport) {
    let report = probe(path).unwrap();
    (
        camera_source(spherecut_media::Source::path(path), &report),
        report,
    )
}

/// `path`'s trailer's metadata, read through the camera crate alone.
fn metadata(path: &Path) -> Metadata {
    let mut f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let trailer = read_trailer(&mut f, len).unwrap().unwrap();
    let bytes = read_record(&mut f, &trailer, 1, MAX_METADATA)
        .unwrap()
        .unwrap();
    parse_metadata(&bytes).unwrap()
}

/// What `path`'s trailer gives a stitch, through the camera crate alone.
fn stitching_of(path: &Path) -> Stitching {
    stitching(&metadata(path)).unwrap().unwrap()
}

/// `path`'s media as `project_media` makes it, of the stream of its
/// first lens track, or its best video stream.
fn media_of(path: &Path, report: &ProbeReport) -> Media {
    let request = VideoRequest {
        stream: report.camera_file.lenses.as_ref().map(|l| l.tracks[0]),
        ..VideoRequest::default()
    };
    let info = FfmpegBackend::new()
        .open_video(path, &request)
        .unwrap()
        .info()
        .clone();
    project_media(spherecut_media::Source::path(path), path, &info).unwrap()
}

/// The dual-fisheye source of `p`'s one clip's media.
fn fisheye(p: &Project) -> &DualFisheye {
    match &p.media_by_id(p.the_clip().media).unwrap().source {
        Source::DualFisheye(d) => d,
        other => panic!("{other:?}"),
    }
}

/// Whether two values are equal, saying both when they aren't: a check
/// a control can hand to `must_fail`.
fn same<T: PartialEq + std::fmt::Debug>(got: &T, want: &T) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{got:?} against {want:?}"))
    }
}

/// Whether two rigs are equal, saying which lenses differ.
fn same_rigs(got: &Rig, want: &Rig) -> Result<(), String> {
    if got == want {
        return Ok(());
    }
    let lenses: Vec<usize> = (0..2)
        .filter(|&b| got.lenses[b] != want.lenses[b])
        .collect();
    Err(format!("lenses {lenses:?} differ, or the seam's numbers"))
}

// The tests.

/// `camera_source` of a synthetic X5 file gives its tracks, its camera as
/// the metadata names it, the table's geometry and the trailer's hash, and
/// a snapshot whose calibration is the trailer's, field for field, from
/// which `rig()` builds, with field 27's offset beside it; the media
/// `project_media` makes of the file is of its first lens track. Control:
/// a file whose V3 string, the one it holds, differs in one digit hashes
/// otherwise.
#[test]
fn a_camera_file_makes_a_dual_fisheye_media() {
    let _time = timed("a_camera_file_makes_a_dual_fisheye_media");
    let control = std::thread::spawn(digit_off_file);
    let offset = std::thread::spawn(offset_file);
    let path = rung3_file();
    let (source, report) = source_of(path);
    let d = source.unwrap().unwrap();
    let lenses = report.camera_file.lenses.as_ref().unwrap();
    assert_eq!(d.tracks.map(|t| t as usize), lenses.tracks);
    assert_eq!(d.front_track, 0);
    assert_eq!(media_of(path, &report).video.stream, d.tracks[0]);
    assert_eq!(
        (&d.tune, &d.gains, &d.profile),
        (&Tune::default(), &Gains::Off {}, &None)
    );
    let meta = metadata(path);
    let camera = report.camera.as_ref().unwrap();
    assert_eq!(d.camera.make.as_deref(), Some("Insta360"));
    assert_eq!(d.camera.make, camera.vendor);
    assert_eq!(
        (&d.camera.model, &d.camera.firmware, &d.camera.serial),
        (&meta.model, &meta.firmware, &meta.serial)
    );
    assert_eq!(
        (
            d.camera.model.as_deref(),
            d.camera.firmware.as_deref(),
            d.camera.serial.as_deref()
        ),
        (
            Some("Insta360 X5"),
            Some("v1.1.22_build1"),
            Some("SYNTH00001")
        )
    );

    // The table's geometry, read by the generation field 136 names: V3,
    // the one string synth's files hold.
    let mut table = known::lookup("Insta360 X5", "v1.1.22_build1").geometry;
    table.generation = Generation::V3;
    assert_eq!(d.geometry, table);
    assert_eq!(
        (d.geometry.status, d.geometry.evidence.as_str()),
        (Status::Provisional, "placeholder until the fit")
    );

    // The snapshot: the strings as stored, their hash as probe printed
    // it, and the calibration the trailer gives, field for field.
    let s = &d.calibration;
    let strings = &meta.calib;
    assert_eq!(
        s.raw,
        RawStrings {
            v1: strings.v1.working.clone(),
            v2: strings.v2.working.clone(),
            v3: strings.v3.working.clone(),
            v6: strings.v6.working.clone(),
        }
    );
    assert!(s.raw.v3.is_some() && s.raw.v6.is_none());
    let probed = report.camera_file.calibration.as_ref().unwrap();
    assert_eq!(s.hash, probed.hash);
    assert_eq!(s.hash, s.raw.hash());
    assert_eq!(s.canvas, probed.canvas);
    let window = probed.window.as_ref().unwrap();
    assert_eq!(s.window, window.dst);
    assert_eq!((s.window_offset, window.offset), ([0, 0], [0, 0]));
    let entry = stitching_of(path);
    assert_eq!(s.calibration(), entry.calibration);
    assert_eq!(s.canvas, [2 * entry.calibration.src, entry.calibration.src]);
    assert!(s.v3.is_some() && s.v6.is_none());
    d.rig([768; 2]).unwrap();

    // Field 27's offset, as probe prints it, kept beside a calibration it
    // doesn't move (U13).
    let (shifted, shifted_report) = source_of(offset.join().unwrap());
    let shifted = shifted.unwrap().unwrap();
    let probed = shifted_report.camera_file.calibration.as_ref().unwrap();
    assert_eq!(
        (
            shifted.calibration.window_offset,
            probed.window.as_ref().unwrap().offset
        ),
        ([32, 16], [32, 16])
    );
    assert_eq!(shifted.calibration.calibration(), s.calibration());

    // Control: a string a digit off hashes otherwise, through the same
    // reading.
    let off = control.join().unwrap();
    let (other, off_report) = source_of(off);
    let other = other.unwrap().unwrap();
    assert_eq!(
        other.calibration.hash,
        off_report.camera_file.calibration.as_ref().unwrap().hash
    );
    must_fail(
        "a V3 string a digit off hashes as the file's",
        same(&other.calibration.hash, &s.hash),
    );
}

/// A two-lens file without a calibration (`InsvFault::NoCalibration`) and
/// a plain MP4 (the same file without its trailer) give no source, so
/// the media `project_media` makes of the first stays equirect. Control:
/// the whole file gives one.
#[test]
fn a_file_without_calibration_stays_equirect() {
    let _time = timed("a_file_without_calibration_stays_equirect");
    let plain = std::thread::spawn(plain_file);
    let (source, report) = source_of(uncalibrated_file());
    assert!(report.camera_file.lenses.is_some(), "two lens tracks");
    assert!(report.camera_file.calibration.is_none());
    assert_eq!(source, Ok(None));
    let media = media_of(uncalibrated_file(), &report);
    assert_eq!(media.source, Source::Equirect {});

    let (source, report) = source_of(plain.join().unwrap());
    assert!(report.camera_file.lenses.is_none(), "no trailer");
    assert_eq!(source, Ok(None));

    // Control.
    let gives_none = |path: &Path| source_of(path).0.unwrap().is_none();
    must_fail("the whole file gives no source", gives_none(rung3_file()));
}

/// `superseded`: a provisional geometry is superseded by a settled entry,
/// or by a provisional one with other evidence; a settled, fitted or user
/// geometry never is (11.4). `superseded_by_table` reads the table's
/// entry for the camera the source names: the source `camera_source`
/// made isn't superseded, one made under an older entry is, and the
/// fixture's, whose binding the user set, isn't. Control: equal evidence
/// gives none.
#[test]
fn a_newer_table_entry_supersedes_a_guess() {
    let _time = timed("a_newer_table_entry_supersedes_a_guess");
    let table = known::lookup("Insta360 X5", "v1.1.22_build1").geometry;
    let with = |status, evidence: &str| Geometry {
        status,
        evidence: evidence.into(),
        ..table.clone()
    };
    let guess = with(Status::Provisional, "a guess before the table's row");
    let measured = with(Status::Settled, "measured on clips 8 to 10");
    let says = |g: &Geometry| {
        Some(Superseded {
            status: g.status,
            evidence: g.evidence.clone(),
        })
    };
    assert_eq!(superseded(&guess, &measured), says(&measured));
    assert_eq!(superseded(&table, &measured), says(&measured));
    assert_eq!(
        superseded(&table, &with(Status::Settled, &table.evidence)),
        says(&with(Status::Settled, &table.evidence))
    );
    assert_eq!(superseded(&guess, &table), says(&table));
    for status in [Status::Settled, Status::Fitted, Status::User] {
        let stored = with(status, "a guess before the table's row");
        for entry in [&table, &measured, &guess] {
            assert_eq!(superseded(&stored, entry), None, "{status:?} by {entry:?}");
        }
    }

    // Against the table, by the camera the source names.
    let d = source_of(rung3_file()).0.unwrap().unwrap();
    assert_eq!(superseded_by_table(&d), None);
    let older = DualFisheye {
        geometry: Geometry {
            evidence: "a guess before the table's row".into(),
            ..d.geometry.clone()
        },
        ..d.clone()
    };
    assert_eq!(
        superseded_by_table(&older),
        Some(Superseded {
            status: Status::Provisional,
            evidence: "placeholder until the fit".into(),
        })
    );
    let mut elsewhere = d.clone();
    elsewhere.camera.firmware = Some("v1.0.0".into());
    let said = superseded_by_table(&elsewhere).unwrap();
    assert!(
        said.evidence.contains("not this file's"),
        "{}",
        said.evidence
    );
    assert_eq!(superseded_by_table(fisheye(&fixture().fixed)), None);

    // Control.
    must_fail(
        "a provisional entry with the same evidence supersedes",
        superseded(&table, &table.clone()).is_some(),
    );
}

/// The most lens B's texel moves from rig `from` to rig `to`, in slot
/// pixels, over 720 directions on the seam: the great circle 90° from
/// lens A's axis in `from`.
fn seam_shift(from: &Rig, to: &Rig) -> f64 {
    let axis = from.lenses[0].lens_from_rig.inverse() * DVec3::Z;
    let (u, v) = axis.any_orthonormal_pair();
    (0..720)
        .map(|i| {
            let (sin, cos) = (f64::from(i) * PI / 360.0).sin_cos();
            let d = u * cos + v * sin;
            let texel = |rig: &Rig| {
                rig.texel(1, d)
                    .unwrap_or_else(|| panic!("lens B has no image of {d}"))
            };
            (texel(to) - texel(from)).length()
        })
        .fold(0.0, f64::max)
}

/// 11.5's export and viewer fixture is `project_media` of the rung-3 file
/// with `camera_source`'s source, then a `set_source` that changes only
/// the geometry's binding (to rung 3's planted Direct) and its status (to
/// User), and one `set_stitch` that changes only the tune and the gains.
/// Its rig is `Rig::build` of the numbers it hands out apart from the
/// project, under synth's calibration and planted conventions through the
/// boundary, as rung 3 builds rung 1's: exactly, since synth prints the
/// calibration's numbers as stored and camera reads them back to the same
/// doubles. Lens B's texel on the seam moves 1 to 5 px from where the
/// default tune puts it. Controls: at `Tune::default()` it moves 0;
/// without the `set_source` (the X5 row's binding, Reversed) the
/// project's rig isn't the boundary's.
#[test]
fn the_stitch_fixture_is_a_set_source_and_a_set_stitch() {
    let _time = timed("the_stitch_fixture_is_a_set_source_and_a_set_stitch");
    let f = fixture();
    assert_eq!(f.spec, InsvSpec::rung3());
    assert_eq!((f.front_track, f.sides), (0, [768, 768]));

    // The numbers, apart from the project.
    let mut tune = Tune {
        feather_deg: 3.0,
        q_per_m: 1.0 / 1.5,
        ..Tune::default()
    };
    tune.lenses[1].ypr_deg = [0.3, 0.0, 0.0];
    assert_eq!(f.tune, tune);
    assert_eq!(
        f.fixed_gains,
        Gains::Fixed {
            rgb: [[1.0; 3], [0.9; 3]]
        }
    );
    let Gains::Auto {
        rate_hz,
        log_ratio,
        basis,
        input,
        ..
    } = &f.auto_gains
    else {
        panic!("{:?}", f.auto_gains);
    };
    assert!((rate_hz - 30000.0 / 1001.0).abs() < 1e-12);
    assert_eq!(log_ratio.len(), 8);
    assert_eq!((log_ratio[0], log_ratio[7]), ([0.0; 3], [0.2; 3]));
    assert!(log_ratio.windows(2).all(|w| w[0][0] < w[1][0]));
    assert_eq!(basis, &gains_basis(&tune));

    // What project_media and camera_source make of the file, made again
    // here: the project both fixtures start from.
    let path = f.files.master.as_path();
    let report = probe(path).unwrap();
    let camera = camera_source(spherecut_media::Source::path(path), &report)
        .unwrap()
        .unwrap();
    let mut entry = media_of(path, &report);
    entry.source = Source::DualFisheye(Box::new(camera.clone()));
    let base = Project::untitled(entry, INITIAL_VIEW).unwrap();
    let id = base.the_clip().media;
    let made = base.media_by_id(id).unwrap();
    let planted = DualFisheye {
        geometry: Geometry {
            binding: Binding::Direct,
            status: Status::User,
            ..camera.geometry.clone()
        },
        ..camera.clone()
    };

    // Each project: that media, then the set_source, which changes only
    // the binding and the status, and the set_stitch, which changes only
    // the tune and the gains. Undoing both gives the first project back.
    for (p, h, gains) in [
        (&f.fixed, &f.fixed_history, &f.fixed_gains),
        (&f.auto, &f.auto_history, &f.auto_gains),
    ] {
        assert_eq!(input.fingerprint, made.fingerprint);
        let labels: Vec<&str> = h.steps().iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, [PLANTED_BINDING, STITCH]);
        assert_eq!(
            h.steps()[0].edits,
            [Edit::SetSource {
                media: id,
                source: Source::DualFisheye(Box::new(planted.clone())),
            }]
        );
        assert_eq!(
            h.steps()[1].edits,
            [Edit::SetStitch {
                media: id,
                tune: Some(tune),
                gains: Some(gains.clone()),
                profile: None,
            }]
        );
        let stitched = DualFisheye {
            tune,
            gains: gains.clone(),
            ..planted.clone()
        };
        assert_eq!(
            p.media_by_id(id).unwrap(),
            &Media {
                source: Source::DualFisheye(Box::new(stitched)),
                ..made.clone()
            }
        );
        let (mut undone, mut history) = (p.clone(), h.clone());
        assert!(history.undo(&mut undone).unwrap().is_some());
        assert!(history.undo(&mut undone).unwrap().is_some());
        assert!(undone.same_content(&base));
    }

    // The rig: synth's calibration and planted conventions, those rung 3's
    // file was written from, through the boundary, under the numbers
    // handed out.
    let truth = rung::camera();
    assert_eq!((&f.spec.calib, &f.spec.rig), (&truth.planted, &truth.truth));
    assert_eq!(
        f.spec.records,
        Records::x5_like(
            std::slice::from_ref(&truth.cal),
            &truth.planted,
            f.spec.frames,
            f.spec.rate
        )
    );
    let reference =
        |tune: &Tune| sphere_rig(&truth.cal, &f.spec.calib, tune, f.front_track, f.sides).unwrap();
    let rig = reference(&f.tune);
    same_rigs(&fisheye(&f.fixed).rig(f.sides).unwrap(), &rig).unwrap();
    same_rigs(&fisheye(&f.auto).rig(f.sides).unwrap(), &rig).unwrap();

    // Lens B on the seam, against the default tune (11.5: 1 to 5 px).
    let untuned = reference(&Tune::default());
    let shift = seam_shift(&untuned, &rig);
    eprintln!("lens B moves {shift:.3} px on the seam");
    assert_eq!(at_least(shift, 1.0).passes(), Some(true), "{shift} px");
    assert_eq!(at_most(shift, 5.0).passes(), Some(true), "{shift} px");

    // Controls: the default tune against itself; and the set_stitch
    // alone, on the X5 row's binding.
    must_fail(
        "lens B moves on the seam at the default tune",
        at_least(seam_shift(&untuned, &untuned), 1.0),
    );
    let mut p = f.fixed.clone();
    let mut h = f.fixed_history.clone();
    h.undo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    p.apply(&f.fixed_history.steps()[1].edits).unwrap();
    assert_eq!(fisheye(&p).geometry.binding, Binding::Reversed);
    must_fail(
        "the set_stitch alone builds the boundary's rig",
        same_rigs(&fisheye(&p).rig(f.sides).unwrap(), &rig),
    );
}
