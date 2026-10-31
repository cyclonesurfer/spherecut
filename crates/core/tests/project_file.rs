//! The project file (M2 plan sections 5 and 11): save and load, the
//! checked-in schema 1 file, refusals, relative paths, and media that has
//! moved or changed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use proptest::prelude::*;
use spherecut_core::project::file::{self, FileError, MediaStatus};
use spherecut_core::project::*;
use spherecut_sphere::perspective::{LensShape, Morph, Projection};

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

/// `text` without its `saved_by` line, which names the build.
fn without_saved_by(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("\"saved_by\""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_checked_in_schema_1_file_loads_and_saves_the_same() {
    let path = data_dir().join("schema1.spherecut");
    let text = file::to_json(&rich(), &data_dir()).unwrap();
    if std::env::var_os("SPHERECUT_BLESS_PROJECT").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let loaded = file::load(&path).unwrap();
    assert_eq!(loaded, rich());
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        without_saved_by(&text),
        without_saved_by(&on_disk),
        "the format changed: a new schema needs a migration, not an edit of this file \
         (SPHERECUT_BLESS_PROJECT=1 rewrites it)"
    );
    // Media paths are written relative to the file's folder.
    assert!(
        on_disk.contains("\"path\": \"../clips/VID_1.mp4\""),
        "{on_disk}"
    );
    assert!(on_disk.starts_with("{\n  \"format\": \"spherecut.project\",\n  \"schema\": 1,\n"));
    assert!(on_disk.ends_with("}\n"));
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
    let e = read(&s.replace("\"schema\": 1", "\"schema\": 3")).unwrap_err();
    assert_eq!(e.code(), "newer_schema");
    assert_eq!(
        e.to_string(),
        "saved by a newer Spherecut (schema 3); this one reads schemas up to 1"
    );
    let e = read(&s.replace("\"schema\": 1", "\"schema\": \"1\"")).unwrap_err();
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
