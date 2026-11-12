//! The project file (M2 plan sections 5 and 11, M3 plan 11.2): save and
//! load, the checked-in schema 1 file, frozen, refusals, relative paths,
//! and media that has moved or changed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use proptest::prelude::*;
use spherecut_core::project::file::{self, FileError, MediaStatus};
use spherecut_core::project::*;
use spherecut_sphere::fisheye::V6Reading;
use spherecut_sphere::perspective::{LensShape, Morph, Projection};
use spherecut_sphere::rig::{
    Binding, CalibTerms, CanvasMap, EulerConv, Extrinsics, Generation, Geometry, LensCalib,
    LensTune, QuarterTurn, Status, TurnAxis, Tune, TzSign,
};
use spherecut_testkit::control::must_fail;

/// The checked-in file, and where its media is said to be.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// A project with something of every kind in it, made by edits.
fn rich() -> Project {
    let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/clips/VID_1.mp4");
    let mut p = project_at(media.to_str().unwrap());
    let c = clip(&p);
    let s = sequence(&p);
    p.apply(&[Edit::AddTarget {
        target: target(&p, "rider", 30.0, -10.0),
    }])
    .unwrap();
    let t = p.targets()[0].id;
    p.apply(&[
        Edit::SetTrim {
            clip: c,
            start: Some(frame(15)),
            end: Some(spherecut_core::Time(
                spherecut_core::Rational::new(29029, 3001).unwrap(),
            )),
        },
        Edit::AddKey {
            clip: c,
            key: orientation_key(frame(60), ypr(90.0, 10.0, 0.0)),
        },
        Edit::AddKey {
            clip: c,
            key: AnyKey::Orientation(Key {
                id: KeyId::NEW,
                at: frame(120),
                value: Orient::Aim {
                    target: t,
                    roll_deg: 2.5,
                },
                ease: Ease {
                    into: Side::Stop,
                    out: Side::Linear,
                    hold: false,
                },
                spin: 1,
            }),
        },
        Edit::AddKey {
            clip: c,
            key: fov_key(frame(60), 62.5),
        },
        Edit::AddKey {
            clip: c,
            key: projection_key(frame(150), Projection::Stereographic),
        },
        Edit::AddKey {
            clip: c,
            key: AnyKey::Projection(Key {
                id: KeyId::NEW,
                at: frame(200),
                value: LensShape::Morph(Morph {
                    from: Projection::Stereographic,
                    to: Projection::Fisheye,
                    morph: 0.25,
                }),
                ease: Ease::STOP,
                spin: 0,
            }),
        },
        Edit::SetOrientMode {
            clip: c,
            mode: OrientMode::Free,
        },
        Edit::AddMarker {
            clip: c,
            marker: Marker {
                id: MarkerId::NEW,
                at: frame(45),
                name: "the jump".into(),
            },
        },
        Edit::SetOutput {
            sequence: s,
            output: Output::Flat {
                aspect: Aspect::Portrait,
                short_side: 1440,
            },
        },
    ])
    .unwrap();
    p
}

/// The schema 2 fixture's project, 11.1's worked example: [`rich`] with
/// a camera file added after its equirect one.
fn schema2() -> Project {
    let mut p = rich();
    p.apply(&[Edit::AddMedia {
        media: camera_media(),
    }])
    .unwrap();
    p
}

/// A camera file of two fisheye lenses, as `media::project::project_media`
/// would make it from an X5-shaped file, with a tune of its own.
///
/// The calibration is no camera's: its V3 and V6 sets are synth's
/// `x5_like` and `x5_like_v6` (`synth/src/lens.rs`) with every number
/// changed in its last printed digits, so that the file holds none of
/// Pat's numbers (F137; `testkit::pats` holds those once). Its raw
/// strings print those sets in `docs/formats/insta360.md` §4.1's token
/// layout and were written by hand: core can call neither synth nor
/// camera, so nothing here parses them. Unit (b)'s `camera_source` tests,
/// in media, are what hold a snapshot's strings to its sets. The geometry
/// is the X5 row's placeholder (`camera/src/insta360/known.rs`), the tune
/// is off the default and validates, the gains are Fixed and the nadir
/// is filled.
fn camera_media() -> Media {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/clips/VID_2.insv");
    // Block 1's centre counts from the canvas's left, its square 5376
    // further on, as camera's `to_calibration` takes it off.
    let src = 5376.0;
    let v3 = [
        LensCalib {
            xi: 2.000003,
            fx: 4284.617,
            fy: 4284.283,
            cx: 2704.806,
            cy: 2673.296,
            ypr_deg: [0.302, -0.197, 90.503],
            t_m: [0.0; 3],
            terms: CalibTerms::V3 {
                k: [0.18632047, 2.04984362, -3.24446911],
                p: [-0.00143571, -0.00339245],
            },
        },
        LensCalib {
            xi: 1.999996,
            fx: 4291.034,
            fy: 4291.027,
            cx: 8045.104 - src,
            cy: 2703.397,
            ypr_deg: [-0.253, 0.348, 89.302],
            t_m: [-0.004457, 0.000019, -0.032004],
            terms: CalibTerms::V3 {
                k: [0.18940831, 2.07073074, -3.24103338],
                p: [0.00013836, -0.00299869],
            },
        },
    ];
    let v6 = [
        LensCalib {
            xi: 2.000002,
            fx: 4277.523,
            fy: 4277.266,
            cx: 2704.812,
            cy: 2673.291,
            ypr_deg: [0.297, -0.204, 90.496],
            t_m: [0.0; 3],
            terms: CalibTerms::V6 {
                d: [
                    0.28032991,
                    0.89045412,
                    2.04927581,
                    -8.05179779,
                    0.0,
                    0.00120207,
                    -0.00259482,
                    0.02324713,
                    0.00533926,
                    -0.01560201,
                    0.00019351,
                    -0.04580529,
                    0.00197182,
                ],
            },
        },
        LensCalib {
            xi: 1.999997,
            fx: 4284.397,
            fy: 4284.764,
            cx: 8045.093 - src,
            cy: 2703.405,
            ypr_deg: [-0.246, 0.353, 89.297],
            t_m: [-0.004463, 0.000014, -0.031996],
            terms: CalibTerms::V6 {
                d: [
                    0.27665339,
                    0.98522968,
                    1.83605261,
                    -7.99374491,
                    0.0,
                    0.00021051,
                    -0.00147632,
                    0.01940419,
                    0.00331844,
                    -0.01330669,
                    0.00074517,
                    -0.02859293,
                    0.00888741,
                ],
            },
        },
    ];
    let raw = RawStrings {
        v1: None,
        v2: None,
        v3: Some(
            "2_2.000003_4284.617_4284.283_2704.806_2673.296_0.302_-0.197_90.503_0.000000_\
             0.000000_0.000000_0.18632047_2.04984362_-3.24446911_-0.00143571_-0.00339245_\
             10752_5376_113_1.999996_4291.034_4291.027_8045.104_2703.397_-0.253_0.348_\
             89.302_-0.004457_0.000019_-0.032004_0.18940831_2.07073074_-3.24103338_\
             0.00013836_-0.00299869_10752_5376_113_197632"
                .into(),
        ),
        v6: Some(
            "2_2.000002_4277.523_4277.266_2704.812_2673.291_0.297_-0.204_90.496_0.000000_\
             0.000000_0.000000_0.28032991_0.89045412_2.04927581_-8.05179779_0.00000000_\
             0.00120207_-0.00259482_0.02324713_0.00533926_-0.01560201_0.00019351_\
             -0.04580529_0.00197182_10752_5376_113_1.999997_4284.397_4284.764_8045.093_\
             2703.405_-0.246_0.353_89.297_-0.004463_0.000014_-0.031996_0.27665339_\
             0.98522968_1.83605261_-7.99374491_0.00000000_0.00021051_-0.00147632_\
             0.01940419_0.00331844_-0.01330669_0.00074517_-0.02859293_0.00888741_10752_\
             5376_113_394240"
                .into(),
        ),
    };
    let hash = raw.hash();
    let lens = |ypr_deg, centre_px, radius_scale, theta_max_deg| LensTune {
        ypr_deg,
        centre_px,
        radius_scale,
        xi_delta: 0.0,
        theta_max_deg,
    };
    Media {
        id: MediaId::NEW,
        path,
        fingerprint: Fingerprint {
            bytes: 2_000_000,
            head: "fedcba9876543210".into(),
        },
        video: MediaVideo {
            stream: 0,
            width: 3840,
            height: 3840,
            rate: spherecut_core::FrameRate::FPS_30,
            duration: spherecut_core::Time::from_secs(10),
        },
        source: Source::DualFisheye(Box::new(DualFisheye {
            camera: Camera {
                make: Some("Insta360".into()),
                model: Some("Insta360 X5".into()),
                firmware: Some("v1.0.31_build2".into()),
                serial: None,
            },
            tracks: [0, 1],
            front_track: 0,
            calibration: CalibSnapshot {
                canvas: [10752, 5376],
                window: [5312, 5312],
                window_offset: [0, 0],
                raw,
                hash,
                v3: Some(v3),
                v6: Some(v6),
            },
            geometry: Geometry {
                binding: Binding::Reversed,
                canvas: CanvasMap::C,
                extrinsics: Extrinsics::Stored(EulerConv::PLACEHOLDER),
                turn: TurnAxis::Y,
                tz: TzSign::AsStored,
                upright: QuarterTurn::Zero,
                generation: Generation::V6,
                v6: V6Reading::RADIAL_ONLY,
                status: Status::Provisional,
                evidence: "placeholder until the fit".into(),
            },
            tune: Tune {
                lenses: [
                    lens([0.4, -0.3, 0.2], [1.5, -2.0], 1.002, 100.0),
                    lens([-0.3, 0.25, -0.1], [-1.0, 0.5], 0.998, 98.0),
                ],
                feather_deg: 4.0,
                q_per_m: 0.5,
            },
            gains: Gains::Fixed {
                rgb: [[1.0, 1.0, 1.0], [0.96, 1.0, 1.03]],
            },
            profile: Some(ProfileRef::of("indoor", b"{\"feather_deg\": 4.0}\n")),
        })),
        caps: Caps {
            nadir: Some(Box::new(Cap {
                mode: CapMode::Fill { ring_deg: 3.0 },
                cap_deg: 20.0,
                edge_deg: 2.0,
                rotation_deg: 0.0,
            })),
            zenith: None,
        },
    }
}

/// `text` without its `saved_by` line, which names the build.
fn without_saved_by(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("\"saved_by\""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The schema 1 file M2 checked in loads, unchanged, as the project it
/// was saved from, its media equirect with no caps (M3 plan 11.2). It is
/// frozen (F063): no switch rewrites it, and a change to the format is a
/// new schema with its own fixture, not an edit of this one.
///
/// Control: the same text with one media number changed doesn't load as
/// that project.
#[test]
fn the_checked_in_schema_1_file_loads_unchanged() {
    let path = data_dir().join("schema1.spherecut");
    let loaded = file::load(&path).unwrap();
    assert_eq!(loaded, rich());
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(file::from_json(&on_disk, &data_dir()).unwrap(), rich());
    let changed = on_disk.replacen("\"bytes\": 1000000", "\"bytes\": 1000001", 1);
    assert_ne!(changed, on_disk);
    must_fail(
        "the file with its media's size changed loads as the project",
        file::from_json(&changed, &data_dir()).is_ok_and(|p| p == rich()),
    );
    // Media paths are written relative to the file's folder.
    assert!(
        on_disk.contains("\"path\": \"../clips/VID_1.mp4\""),
        "{on_disk}"
    );
    assert!(on_disk.starts_with("{\n  \"format\": \"spherecut.project\",\n  \"schema\": 1,\n"));
    assert!(on_disk.ends_with("}\n"));
}

/// The schema 2 fixture, `schema2.spherecut` (M3 plan 11.2), holds a
/// dual-fisheye source and loads and saves the same; it alone may be
/// blessed, through F055's `testkit::bless` once M3.7 has merged (F063).
/// Marked schema 3 it is refused as newer.
///
/// Control: the schema 1 file's text isn't what a save writes.
#[test]
fn the_checked_in_schema_2_file_loads_and_saves_the_same() {
    let path = data_dir().join("schema2.spherecut");
    let text = file::to_json(&schema2(), &data_dir()).unwrap();
    // The variable's presence blesses, as M2's schema 1 test read it;
    // M3.7's F055 brings `testkit::bless`, whose one rule reads unset,
    // empty and "0" as off, and takes this switch over when it merges.
    if std::env::var_os("SPHERECUT_BLESS_PROJECT").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        without_saved_by(&text),
        without_saved_by(&on_disk),
        "schema2.spherecut isn't what a save writes: a change within schema 2 rewrites it \
         (SPHERECUT_BLESS_PROJECT=1), a new schema gets its own fixture"
    );
    let loaded = file::load(&path).unwrap();
    assert_eq!(loaded, schema2());
    // A save of what it loads writes it again.
    let again = file::to_json(&loaded, &data_dir()).unwrap();
    assert_eq!(without_saved_by(&again), without_saved_by(&on_disk));
    for key in [
        "\"schema\": 2,",
        "\"kind\": \"dual_fisheye\"",
        "\"q_per_m\": 0.5",
        "\"window_offset\"",
        "\"path\": \"../clips/VID_2.insv\"",
    ] {
        assert!(on_disk.contains(key), "{key}");
    }
    let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(
        value["project"]["media"][1]["source"]["geometry"]["extrinsics"],
        serde_json::json!({"stored": {
            "axes": ["y", "x", "z"],
            "order": ["yaw", "pitch", "roll"],
            "signs": [1, 1, 1],
            "roll_offset": -90
        }}),
        "11.1's extrinsics"
    );
    let three = on_disk.replacen("\"schema\": 2,", "\"schema\": 3,", 1);
    assert!(matches!(
        file::from_json(&three, &data_dir()),
        Err(FileError::NewerSchema { found: 3 })
    ));

    let schema1 = std::fs::read_to_string(data_dir().join("schema1.spherecut")).unwrap();
    let saved = file::to_json(&rich(), &data_dir()).unwrap();
    must_fail(
        "the schema 1 file taken for what a save writes",
        without_saved_by(&schema1) == without_saved_by(&saved),
    );
}

/// A schema 2 media without a `source` is refused, naming its path, so a
/// damaged file never defaults to equirect silently (11.2); a schema 1
/// media without one reads as equirect.
#[test]
#[ignore = "M3.9 unit (a)"]
fn a_media_without_source_is_refused_by_path() {
    todo!()
}

/// `deny_unknown_fields` still refuses a misspelt key in a source's
/// `tune` (11.5), with a message that names the key and gives a line and
/// column. serde reads a source whole to find its `kind` before its
/// fields, so an error inside one is placed at the source's closing
/// brace, not on the key's own line: the test checks that a position is
/// given, not which.
#[test]
#[ignore = "M3.9 unit (a)"]
fn a_misspelt_key_in_tune_is_refused() {
    todo!()
}

/// A project file in a folder reached through a link with a `../` target
/// finds its media from the file's real folder, while media paths stay
/// lexical as D47 intends (F031; the CLI's half is cli's
/// `a_project_in_a_linked_folder_renders_its_media`).
///
/// Two layouts, each read through `file::load` and
/// `ProjectFolder::of_file`: `link` names `real/sub`, whose project
/// keeps its media in `real` as `../clip.mp4`; and `home/proj` names
/// `nas/proj`, whose `current.spherecut` is a link to
/// `../archive/v3.spherecut`, kept with its media in `nas/archive`.
///
/// Control: the base's rule, the link's own folder taken lexically, puts
/// the media beside the link's parent, where it isn't.
#[cfg(unix)]
#[test]
fn a_project_in_a_linked_folder_finds_media_by_its_real_folder() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("linked-folder");
    // Where the system's temp folder is reached through a link, its
    // real path.
    let root = std::fs::canonicalize(&dir.0).unwrap();
    let root = root.as_path();
    std::fs::create_dir_all(root.join("real/sub")).unwrap();
    std::fs::create_dir_all(root.join("nas/proj")).unwrap();
    std::fs::create_dir_all(root.join("nas/archive")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    symlink("real/sub", root.join("link")).unwrap();
    symlink("../nas/proj", root.join("home/proj")).unwrap();
    symlink(
        "../archive/v3.spherecut",
        root.join("nas/proj/current.spherecut"),
    )
    .unwrap();
    let cases = [
        // (the path a user gives, where the project is, its media)
        (
            root.join("link/p.spherecut"),
            root.join("real/sub/p.spherecut"),
            root.join("real/clip.mp4"),
        ),
        (
            root.join("home/proj/current.spherecut"),
            root.join("nas/archive/v3.spherecut"),
            root.join("nas/archive/clip.mp4"),
        ),
    ];
    for (given, real, media) in &cases {
        std::fs::write(media, b"a clip").unwrap();
        let p = project_at(media.to_str().unwrap());
        file::save(&p, real).unwrap();
        let text = std::fs::read_to_string(real).unwrap();
        assert!(!text.contains(root.to_str().unwrap()), "relative: {text}");

        let loaded = file::load(given).unwrap();
        assert_eq!(&loaded.media()[0].path, media, "{}", given.display());
        assert!(loaded.media()[0].path.exists());
        let (folder, name) = ProjectFolder::of_file(given).unwrap();
        assert_eq!(folder.path(), real.parent().unwrap(), "{}", given.display());
        assert_eq!(name, real.file_name().unwrap());
        let loaded = folder.load(&name).unwrap().project;
        assert_eq!(&loaded.media()[0].path, media, "{}", given.display());

        // The base's rule: the file's folder lexically from the link.
        let lexical = file::lexical(&spherecut_core::fs::follow_links(given).unwrap());
        let rel = Path::new(text.split("\"path\": \"").nth(1).unwrap().split('"').next().unwrap());
        let there = file::lexical(&lexical.parent().unwrap().join(rel));
        must_fail(
            &format!("{}'s media found lexically", given.display()),
            there.exists(),
        );
    }
}

/// `file::load` behaves as `ProjectFolder::load` does (F034): it opens
/// without blocking, refuses what isn't a regular file (a FIFO at the
/// path), and calls a file that isn't UTF-8 `not_project`. The FIFO is
/// loaded on a thread held to a deadline, so a load that waits for a
/// writer fails the test rather than hanging it.
///
/// Control: a regular UTF-8 project at the same path loads.
#[cfg(unix)]
#[test]
fn a_fifo_or_bad_utf8_at_the_path_is_not_a_project() {
    use std::sync::mpsc;
    use std::time::Duration;

    use rustix::fs::{CWD, FileType, Mode, mknodat};
    let dir = TempDir::new("fifo");
    let path = dir.0.join("p.spherecut");
    mknodat(CWD, &path, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();
    let (tx, rx) = mpsc::channel();
    let fifo = path.clone();
    std::thread::spawn(move || {
        tx.send(file::load(&fifo).map(drop)).ok();
    });
    let e = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(r) => r.unwrap_err(),
        Err(_) => {
            // Let the stuck open go before failing.
            std::fs::OpenOptions::new().write(true).open(&path).ok();
            panic!("loading a FIFO waited for a writer for 5 s");
        }
    };
    assert_eq!(e.code(), "not_project", "{e}");
    assert!(e.to_string().contains("not a regular file"), "{e}");
    std::fs::remove_file(&path).unwrap();

    std::fs::write(&path, b"{\"format\": \"spherecut.project\", \"name\": \"\xff\"}").unwrap();
    let e = file::load(&path).unwrap_err();
    assert_eq!(e.code(), "not_project", "{e}");
    assert!(e.to_string().contains("UTF-8"), "{e}");

    std::fs::write(&path, sample()).unwrap();
    assert!(file::load(&path).is_ok());
    must_fail(
        "a regular UTF-8 project refused as not a project",
        file::load(&path).is_err_and(|e| e.code() == "not_project"),
    );
}

#[test]
fn save_then_load_is_equal() {
    let dir = TempDir::new("save");
    let p = rich();
    let path = dir.0.join("sub/../a.spherecut");
    std::fs::create_dir_all(dir.0.join("sub")).unwrap();
    file::save(&p, &path).unwrap();
    assert_eq!(file::load(&path).unwrap(), p);
    // A second save replaces the first, leaving no temporary file.
    let mut q = p.clone();
    let s = sequence(&q);
    q.apply(&[Edit::Rename {
        id: s.0,
        name: "Second".into(),
    }])
    .unwrap();
    file::save(&q, &path).unwrap();
    assert_eq!(file::load(&path).unwrap(), q);
    let names: Vec<String> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !spherecut_core::fs::is_temp_name(n)),
        "{names:?}"
    );
}

#[test]
fn every_float_reads_back_bit_for_bit() {
    let mut p = project();
    let c = clip(&p);
    let mut keys = Vec::new();
    for n in 0..200i64 {
        let x = (n as f64 * 0.7368).sin();
        keys.push(Key {
            id: KeyId::NEW,
            at: frame(n),
            value: Orient::View(ypr(x * 179.9, x * 89.0, (x * 7.0).cos() * 33.3)),
            ease: Ease::SMOOTH,
            spin: 0,
        });
    }
    p.apply(&[Edit::SetKeys {
        clip: c,
        keys: AnyKeys::Orientation(keys),
    }])
    .unwrap();
    let text = file::to_json(&p, Path::new("/clips")).unwrap();
    let back = file::from_json(&text, Path::new("/clips")).unwrap();
    assert_eq!(back, p);
    for (a, b) in back
        .the_clip()
        .view
        .orientation
        .keys
        .iter()
        .zip(&p.the_clip().view.orientation.keys)
    {
        let (Orient::View(a), Orient::View(b)) = (a.value, b.value) else {
            unreachable!()
        };
        assert_eq!(
            a.quat().to_array().map(f64::to_bits),
            b.quat().to_array().map(f64::to_bits)
        );
    }
}

fn read(text: &str) -> Result<Project, FileError> {
    file::from_json(text, Path::new("/clips"))
}

fn sample() -> String {
    file::to_json(&project(), Path::new("/clips")).unwrap()
}

#[test]
fn refusals() {
    let s = sample();
    let e = read("not json").unwrap_err();
    assert_eq!(e.code(), "not_project");
    let e = read(r#"{"format": "something.else", "schema": 1}"#).unwrap_err();
    assert_eq!(e.code(), "not_project");
    assert!(e.to_string().contains("something.else"), "{e}");
    let e = read(&s.replace("\"schema\": 2", "\"schema\": 3")).unwrap_err();
    assert_eq!(e.code(), "newer_schema");
    assert_eq!(
        e.to_string(),
        "saved by a newer Spherecut (schema 3); this one reads schemas up to 2"
    );
    let e = read(&s.replace("\"schema\": 2", "\"schema\": \"2\"")).unwrap_err();
    assert_eq!(e.code(), "not_project");
    // A typo in a hand edit is caught, with where it is.
    let e = read(&s.replace("\"short_side\"", "\"shortside\"")).unwrap_err();
    assert_eq!(e.code(), "malformed");
    assert!(
        e.to_string().contains("shortside") && e.to_string().contains("line"),
        "{e}"
    );
    let e = read(&s.replace("\"revision\": 0,", "\"revision\": 0, \"extra\": 1,")).unwrap_err();
    assert_eq!(e.code(), "malformed");
    // A file that breaks a rule is refused by validate.
    let e = read(&s.replace("\"value\": 90.0", "\"value\": 4.0")).unwrap_err();
    assert_eq!(e.code(), "invalid");
    let e = read(&s.replace("\"next_id\": 8", "\"next_id\": 3")).unwrap_err();
    assert_eq!(e.code(), "invalid");
}

#[test]
fn hand_edits_of_the_readable_copies() {
    let s = sample();
    // The clip's end is 10 s: `"exact": "10", "seconds": 10.0`.
    let mut v: serde_json::Value = serde_json::from_str(&s).unwrap();
    fn end(v: &mut serde_json::Value) -> &mut serde_json::Value {
        &mut v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"]
    }
    assert_eq!(
        *end(&mut v),
        serde_json::json!({"exact": "10", "seconds": 10.0})
    );
    *end(&mut v) = serde_json::json!({"exact": "10", "seconds": 8.0});
    let e = read(&v.to_string()).unwrap_err();
    assert_eq!(e.code(), "malformed");
    let msg = e.to_string();
    assert!(
        msg.contains("at /project/sequences/0/tracks/0/clips/0/source/end:"),
        "{msg}"
    );
    assert!(msg.contains("edit it"), "{msg}");
    // Editing `exact` and leaving `seconds` is caught the same way.
    *end(&mut v) = serde_json::json!({"exact": "8", "seconds": 10.0});
    assert_eq!(read(&v.to_string()).unwrap_err().code(), "malformed");
    // `seconds` left out is fine; `exact` left out is not.
    v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"] =
        serde_json::json!({"exact": "8"});
    let p = read(&v.to_string()).unwrap();
    assert_eq!(p.the_clip().source.end, spherecut_core::Time::from_secs(8));
    v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"] =
        serde_json::json!({"seconds": 8.0});
    let e = read(&v.to_string()).unwrap_err();
    assert!(e.to_string().contains("needs \"exact\""), "{e}");
    // Anything beside them would be dropped unread, so it is refused: a
    // note, or a misspelt field next to an edited `seconds`.
    for extra in [
        serde_json::json!({"exact": "10", "seconds": 10.0, "note": 1}),
        serde_json::json!({"exact": "10", "seconds": 8.0, "frames": 240}),
        serde_json::json!({"exact": "10", "frame": 300}),
    ] {
        v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"] = extra.clone();
        let e = read(&v.to_string()).unwrap_err();
        assert_eq!(e.code(), "malformed", "{extra}");
        assert!(
            e.to_string().contains("\"exact\" and \"seconds\" only"),
            "{e}"
        );
    }
    // `seconds` is checked to 1 µs, an hour in as at the start.
    v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"] =
        serde_json::json!({"exact": "3600", "seconds": 3600.003});
    let e = read(&v.to_string()).unwrap_err();
    assert!(e.to_string().contains("edit it"), "{e}");
    v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"] =
        serde_json::json!({"exact": "10", "seconds": 10.0000005});
    assert!(read(&v.to_string()).is_ok());
    // An orientation's `ypr_deg` edited alone is refused; given alone, it
    // is read.
    let key = &mut v["project"]["sequences"][0]["tracks"][0]["clips"][0]["view"]["orientation"][0]
        ["value"]["view"];
    key["ypr_deg"] = serde_json::json!([45.0, 0.0, 0.0]);
    v["project"]["sequences"][0]["tracks"][0]["clips"][0]["source"]["end"] =
        serde_json::json!({"exact": "10"});
    let e = read(&v.to_string()).unwrap_err();
    assert!(e.to_string().contains("disagree"), "{e}");
    let key = &mut v["project"]["sequences"][0]["tracks"][0]["clips"][0]["view"]["orientation"][0]
        ["value"]["view"];
    key.as_object_mut().unwrap().remove("quat");
    let p = read(&v.to_string()).unwrap();
    let Orient::View(o) = p.the_clip().view.orientation.keys[0].value else {
        unreachable!()
    };
    assert!((o.ypr().yaw.to_degrees() - 45.0).abs() < 1e-9);
}

#[test]
fn media_paths() {
    let p = project_at("/v/clips/a.mp4");
    let text = file::to_json(&p, Path::new("/v/projects/2026")).unwrap();
    assert!(text.contains("\"path\": \"../../clips/a.mp4\""), "{text}");
    // Saved from another folder, the paths are rewritten for it.
    let back = file::from_json(&text, Path::new("/v/projects/2026")).unwrap();
    assert_eq!(back.media()[0].path, Path::new("/v/clips/a.mp4"));
    let text = file::to_json(&back, Path::new("/home/pat")).unwrap();
    assert!(text.contains("\"path\": \"../../v/clips/a.mp4\""), "{text}");
    // An absolute path in a file (written by hand or by an agent) is read
    // as it is.
    let abs = text.replace("../../v/clips/a.mp4", "/srv/a.mp4");
    let back = file::from_json(&abs, Path::new("/home/pat")).unwrap();
    assert_eq!(back.media()[0].path, Path::new("/srv/a.mp4"));
    // A path that isn't UTF-8 can't be written.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bad = std::ffi::OsStr::from_bytes(b"/v/\xff.mp4");
        let p = project_at("/v/x.mp4");
        let mut m = p.media()[0].clone();
        m.path = PathBuf::from(bad);
        m.id = MediaId::NEW;
        let mut q = p.clone();
        q.apply(&[Edit::AddMedia { media: m }]).unwrap();
        let e = file::to_json(&q, Path::new("/v")).unwrap_err();
        assert!(matches!(e, FileError::PathNotUtf8 { .. }), "{e}");
    }
    // A path with `..` in it is made lexical when a project starts from
    // it, as a load would make it, and refused in an edit.
    let p = project_at("/v/work/../clips/./a.mp4");
    assert_eq!(p.media()[0].path, Path::new("/v/clips/a.mp4"));
    let mut m = p.media()[0].clone();
    m.path = PathBuf::from("/v/work/../b.mp4");
    m.id = MediaId::NEW;
    let mut q = p.clone();
    let e = q.apply(&[Edit::AddMedia { media: m }]).unwrap_err();
    assert!(e.problem.to_string().contains("/v/b.mp4"), "{e:?}");
    assert_eq!(q, p);
}

#[cfg(unix)]
#[test]
fn a_project_saved_through_a_link_keeps_its_paths_from_the_files_folder() {
    let dir = TempDir::new("link");
    let (home, nas) = (dir.0.join("home"), dir.0.join("nas/proj"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&nas).unwrap();
    let link = home.join("p.spherecut");
    std::os::unix::fs::symlink(nas.join("p.spherecut"), &link).unwrap();
    let media = home.join("clips/a.mp4");
    let p = project_at(media.to_str().unwrap());
    file::save(&p, &link).unwrap();
    let text = std::fs::read_to_string(nas.join("p.spherecut")).unwrap();
    // From nas/proj to home/clips, not from home.
    assert!(
        text.contains("\"path\": \"../../home/clips/a.mp4\""),
        "{text}"
    );
    for path in [&link, &nas.join("p.spherecut")] {
        let back = file::load(path).unwrap();
        assert_eq!(back.media()[0].path, media, "{}", path.display());
    }
}

#[test]
fn a_file_with_a_revision_past_the_ids_is_refused() {
    let mut v: serde_json::Value = serde_json::from_str(&sample()).unwrap();
    v["project"]["revision"] = serde_json::json!(u64::MAX);
    let e = read(&v.to_string()).unwrap_err();
    assert!(e.to_string().contains("revision"), "{e}");
}

#[test]
fn moved_and_changed_media() {
    let dir = TempDir::new("media");
    let clip_path = dir.0.join("clip.mp4");
    std::fs::write(&clip_path, vec![7u8; 3000]).unwrap();
    let mut m = common::media(clip_path.to_str().unwrap(), 10);
    m.fingerprint = Fingerprint {
        bytes: 3000,
        head: file::head_hash(&clip_path).unwrap(),
    };
    let p = Project::untitled(
        m,
        InitialView {
            orientation: Orientation::IDENTITY,
            fov_deg: 90.0,
            projection: Projection::Rectilinear,
        },
    )
    .unwrap();
    let project_path = dir.0.join("p.spherecut");
    file::save(&p, &project_path).unwrap();
    assert_eq!(file::media_status(&p), [MediaStatus::Present]);
    // Changed contents, same size.
    let mut bytes = vec![7u8; 3000];
    bytes[10] = 8;
    std::fs::write(&clip_path, &bytes).unwrap();
    let loaded = file::load(&project_path).unwrap();
    assert!(
        matches!(&file::media_status(&loaded)[0], MediaStatus::Changed(w) if w.contains("first MiB"))
    );
    std::fs::write(&clip_path, vec![7u8; 10]).unwrap();
    assert!(
        matches!(&file::media_status(&loaded)[0], MediaStatus::Changed(w) if w.contains("size"))
    );
    // Gone: the project still opens, with the clip offline.
    std::fs::remove_file(&clip_path).unwrap();
    let loaded = file::load(&project_path).unwrap();
    assert_eq!(file::media_status(&loaded), [MediaStatus::Missing]);
}

#[test]
fn too_large_and_missing_files() {
    let dir = TempDir::new("large");
    let big = dir.0.join("big.spherecut");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(file::MAX_FILE_BYTES + 1).unwrap();
    assert_eq!(file::load(&big).unwrap_err().code(), "too_large");
    assert_eq!(
        file::load(&dir.0.join("none.spherecut"))
            .unwrap_err()
            .code(),
        "not_found"
    );
}

/// A JSON value from the shapes a project file holds, for the mutations.
fn junk() -> impl Strategy<Value = serde_json::Value> {
    use serde_json::{Value, json};
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<u64>().prop_map(|n| json!(n)),
        prop::num::f64::ANY.prop_map(|x| json!(x)),
        "[a-z_/0-9.:-]{0,12}".prop_map(Value::String),
        Just(json!("1/0")),
        Just(json!("-1/3")),
        Just(json!({"exact": "9223372036854775807/1"})),
        Just(json!({"quat": [0, 0, 0, 0]})),
        Just(json!({"ypr_deg": [1e308, 0, 0]})),
        Just(json!({"from": "fisheye", "to": "fisheye", "morph": 0.5})),
    ];
    leaf.prop_recursive(2, 8, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            prop::collection::btree_map("[a-z_]{1,8}", inner, 0..3)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Every place in `v`, as JSON pointers.
fn pointers(v: &serde_json::Value, at: String, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                pointers(x, format!("{at}/{i}"), out);
            }
        }
        serde_json::Value::Object(m) => {
            for (k, x) in m {
                pointers(x, format!("{at}/{k}"), out);
            }
        }
        _ => {}
    }
    out.push(at);
}

proptest! {
    /// Files written by agents and by hand: whatever is changed, deleted
    /// or cut short, loading gives a project that passes `validate` or a
    /// typed error, never a panic.
    #[test]
    fn mangled_files_load_or_are_refused(
        changes in prop::collection::vec((any::<prop::sample::Index>(), junk(), 0u8..4), 1..4),
        cut in any::<prop::sample::Index>(),
        cut_it in prop::bool::weighted(0.1),
    ) {
        let text = std::fs::read_to_string(data_dir().join("schema1.spherecut")).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for (at, value, how) in changes {
            let mut all = Vec::new();
            pointers(&v, String::new(), &mut all);
            let p = at.get(&all).clone();
            match how {
                // Replace a value.
                0 | 1 => {
                    if let Some(slot) = v.pointer_mut(&p) {
                        *slot = value;
                    }
                }
                // Delete a field or an element.
                2 => {
                    if let Some((parent, last)) = p.rsplit_once('/') {
                        match v.pointer_mut(parent) {
                            Some(serde_json::Value::Object(m)) => {
                                m.remove(last);
                            }
                            Some(serde_json::Value::Array(a)) => {
                                if let Ok(i) = last.parse::<usize>() {
                                    a.remove(i);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Duplicate an element of an array.
                _ => {
                    if let Some((parent, last)) = p.rsplit_once('/')
                        && let Some(serde_json::Value::Array(a)) = v.pointer_mut(parent)
                        && let Ok(i) = last.parse::<usize>()
                    {
                        let x = a[i].clone();
                        a.insert(i, x);
                    }
                }
            }
        }
        let mut mangled = serde_json::to_string_pretty(&v).unwrap();
        if cut_it {
            let mut n = cut.index(mangled.len());
            while !mangled.is_char_boundary(n) {
                n -= 1;
            }
            mangled.truncate(n);
        }
        if let Ok(p) = file::from_json(&mangled, &data_dir()) {
            prop_assert!(p.validate().is_ok());
            prop_assert!(file::to_json(&p, &data_dir()).is_ok());
        }
    }
}
