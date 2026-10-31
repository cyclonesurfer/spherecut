//! Every `Edit` variant applied, undone, redone and refused (M2 plan
//! sections 4 and 11).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use spherecut_core::Time;
use spherecut_core::project::*;
use spherecut_sphere::perspective::Projection;

/// Record `edits` as one step, check the revision went up by one, undo
/// it and check the project is as it was, redo it and check it is as it
/// was after. Returns the project after.
fn round_trip(p: &mut Project, edits: Vec<Edit>) -> Project {
    let mut h = History::new();
    let before = p.clone();
    h.record(p, "test", Origin::User, &edits)
        .unwrap_or_else(|e| panic!("{edits:?}: {e}"));
    assert_eq!(p.revision(), before.revision() + 1);
    let after = p.clone();
    h.undo(p).unwrap().unwrap();
    assert!(p.same_content(&before), "undo of {edits:?}");
    assert_eq!(p.revision(), before.revision() + 2);
    assert_eq!(
        p.next_id(),
        after.next_id(),
        "undo never lowers the id counter"
    );
    h.redo(p).unwrap().unwrap();
    assert!(p.same_content(&after), "redo of {edits:?}");
    // The step's edits and inverse read back from JSON as they were.
    let step = h.undo_step().unwrap();
    for e in step.edits.iter().chain(&step.inverse) {
        let json = serde_json::to_string(e).unwrap();
        let back: Edit = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, e, "{json}");
    }
    after
}

/// `edits` are refused with `code` at `index`, and nothing changes.
fn refused(p: &mut Project, edits: Vec<Edit>, index: usize, code: Code) -> EditError {
    let before = p.clone();
    let e = p.apply(&edits).expect_err("should be refused");
    assert_eq!((e.index, e.code()), (index, code), "{e}");
    assert_eq!(
        *p, before,
        "a refused edit changes nothing, revision included"
    );
    e
}

#[test]
fn media() {
    let mut p = project();
    let after = round_trip(
        &mut p,
        vec![Edit::AddMedia {
            media: common::media("/clips/VID_2.mp4", 5),
        }],
    );
    assert_eq!(after.media().len(), 2);
    let added = after.media()[1].id;
    assert_eq!(added.0, 8, "the new id comes from the counter");
    round_trip(&mut p, vec![Edit::RemoveMedia { media: added }]);
    let used = p.the_clip().media;
    refused(
        &mut p,
        vec![Edit::RemoveMedia { media: used }],
        0,
        Code::InUse,
    );
    refused(
        &mut p,
        vec![Edit::RemoveMedia {
            media: MediaId(999),
        }],
        0,
        Code::NotFound,
    );
    let mut m = common::media("/clips/VID_3.mp4", 5);
    m.id = MediaId(3);
    refused(&mut p, vec![Edit::AddMedia { media: m }], 0, Code::IdGiven);
    refused(
        &mut p,
        vec![Edit::AddMedia {
            media: common::media("relative.mp4", 5),
        }],
        0,
        Code::Invalid,
    );
}

#[test]
fn clips_are_one_in_m2() {
    let mut p = project();
    let track = p.the_sequence().tracks[0].id;
    let mut c = p.the_clip().clone();
    c.id = ClipId::NEW;
    for k in &mut c.view.orientation.keys {
        k.id = KeyId::NEW;
    }
    for k in &mut c.view.fov.keys {
        k.id = KeyId::NEW;
    }
    for k in &mut c.view.projection.keys {
        k.id = KeyId::NEW;
    }
    refused(
        &mut p,
        vec![Edit::AddClip {
            track,
            clip: c.clone(),
        }],
        0,
        Code::Limit,
    );
    refused(
        &mut p,
        vec![Edit::AddClip {
            track: TrackId(999),
            clip: c,
        }],
        0,
        Code::NotFound,
    );
    let id = clip(&p);
    refused(&mut p, vec![Edit::RemoveClip { clip: id }], 0, Code::Limit);
}

#[test]
fn trim() {
    let mut p = project();
    let c = clip(&p);
    let after = round_trip(
        &mut p,
        vec![Edit::SetTrim {
            clip: c,
            start: Some(frame(30)),
            end: Some(frame(240)),
        }],
    );
    assert_eq!(after.the_clip().source.start, frame(30));
    assert_eq!(after.the_clip().duration(), frame(210));
    round_trip(
        &mut p,
        vec![Edit::SetTrim {
            clip: c,
            start: None,
            end: Some(frame(299)),
        }],
    );
    let set = |start, end| Edit::SetTrim {
        clip: c,
        start,
        end,
    };
    refused(
        &mut p,
        vec![set(Some(frame(299)), None)],
        0,
        Code::OutOfRange,
    );
    refused(
        &mut p,
        vec![set(None, Some(frame(301)))],
        0,
        Code::OutOfRange,
    );
    refused(
        &mut p,
        vec![set(Some(frame(-1)), None)],
        0,
        Code::OutOfRange,
    );
    refused(&mut p, vec![set(None, None)], 0, Code::Invalid);
    refused(
        &mut p,
        vec![Edit::SetTrim {
            clip: ClipId(999),
            start: None,
            end: Some(frame(1)),
        }],
        0,
        Code::NotFound,
    );
    // The second edit fails, so the first is not applied either.
    refused(
        &mut p,
        vec![set(Some(frame(10)), None), set(None, Some(frame(5)))],
        1,
        Code::OutOfRange,
    );
}

#[test]
fn output_rate_and_export() {
    let mut p = project();
    let s = sequence(&p);
    let equirect = Output::Equirect {
        width: 5760,
        orientation: ypr(90.0, 0.0, 0.0),
    };
    let after = round_trip(
        &mut p,
        vec![Edit::SetOutput {
            sequence: s,
            output: equirect,
        }],
    );
    assert_eq!(after.the_sequence().output.size(), (5760, 2880));
    for output in [
        Output::Flat {
            aspect: Aspect::Wide,
            short_side: 1000,
        },
        Output::Equirect {
            width: 4096,
            orientation: Orientation::IDENTITY,
        },
    ] {
        refused(
            &mut p,
            vec![Edit::SetOutput {
                sequence: s,
                output,
            }],
            0,
            Code::OutOfRange,
        );
    }
    round_trip(
        &mut p,
        vec![Edit::SetRate {
            sequence: s,
            rate: spherecut_core::FrameRate::FPS_25,
        }],
    );
    let fast =
        spherecut_core::FrameRate::new(spherecut_core::Rational::from_integer(1000)).unwrap();
    refused(
        &mut p,
        vec![Edit::SetRate {
            sequence: s,
            rate: fast,
        }],
        0,
        Code::OutOfRange,
    );
    let mut export = p.the_sequence().export;
    export.codec = Codec::Av1;
    export.motion_blur = true;
    round_trip(
        &mut p,
        vec![Edit::SetExport {
            sequence: s,
            export,
        }],
    );
    refused(
        &mut p,
        vec![Edit::SetRate {
            sequence: SequenceId(999),
            rate: spherecut_core::FrameRate::FPS_25,
        }],
        0,
        Code::NotFound,
    );
    // An unknown codec name is refused when the edit is read.
    let json = r#"{"op": "set_export", "sequence": 2, "export": {"codec": "h266", "quality": "high", "encoder": "auto", "audio": "copy", "motion_blur": false}}"#;
    let e = serde_json::from_str::<Edit>(json).unwrap_err();
    assert!(e.to_string().contains("h266"), "{e}");
}

#[test]
fn keys() {
    let mut p = project();
    let c = clip(&p);
    let after = round_trip(
        &mut p,
        vec![
            Edit::AddKey {
                clip: c,
                key: orientation_key(frame(60), ypr(90.0, 10.0, 0.0)),
            },
            Edit::AddKey {
                clip: c,
                key: fov_key(frame(60), 60.0),
            },
            Edit::AddKey {
                clip: c,
                key: projection_key(frame(90), Projection::Fisheye),
            },
        ],
    );
    let v = &after.the_clip().view;
    assert_eq!(
        (
            v.orientation.keys.len(),
            v.fov.keys.len(),
            v.projection.keys.len()
        ),
        (2, 2, 2)
    );
    let moved = v.orientation.keys[1].id;
    assert_eq!(moved.0, 8);
    // Moved past the second key: the keys stay in time order.
    let first = p.the_clip().view.orientation.keys[0].id;
    let after = round_trip(
        &mut p,
        vec![Edit::SetKey {
            key: first,
            at: Some(frame(120)),
            value: None,
            ease: Some(Ease::STOP),
            spin: Some(1),
        }],
    );
    let keys = &after.the_clip().view.orientation.keys;
    assert_eq!(
        keys.iter().map(|k| k.at).collect::<Vec<_>>(),
        [frame(60), frame(120)]
    );
    assert_eq!(
        (keys[1].id, keys[1].spin, keys[1].ease),
        (first, 1, Ease::STOP)
    );
    round_trip(
        &mut p,
        vec![Edit::SetKey {
            key: moved,
            at: None,
            value: Some(AnyValue::Orientation(Orient::View(ypr(-30.0, 0.0, 0.0)))),
            ease: None,
            spin: None,
        }],
    );
    round_trip(&mut p, vec![Edit::RemoveKey { key: moved }]);

    let fov = p.the_clip().view.fov.keys[0].id;
    let set_value = |key, v| Edit::SetKey {
        key,
        at: None,
        value: Some(v),
        ease: None,
        spin: None,
    };
    refused(
        &mut p,
        vec![set_value(fov, AnyValue::Projection(Projection::Fisheye))],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![set_value(fov, AnyValue::Fov(5.0))],
        0,
        Code::OutOfRange,
    );
    refused(
        &mut p,
        vec![set_value(fov, AnyValue::Fov(f64::NAN))],
        0,
        Code::OutOfRange,
    );
    refused(
        &mut p,
        vec![Edit::SetKey {
            key: fov,
            at: None,
            value: None,
            ease: None,
            spin: Some(2),
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::SetKey {
            key: fov,
            at: None,
            value: None,
            ease: None,
            spin: None,
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::SetKey {
            key: KeyId(999),
            at: Some(frame(1)),
            value: None,
            ease: None,
            spin: None,
        }],
        0,
        Code::NotFound,
    );
    let at60 = p.the_clip().view.fov.keys[1].id;
    refused(
        &mut p,
        vec![Edit::SetKey {
            key: at60,
            at: Some(Time::ZERO),
            value: None,
            ease: None,
            spin: None,
        }],
        0,
        Code::Exists,
    );
    refused(
        &mut p,
        vec![Edit::AddKey {
            clip: c,
            key: fov_key(frame(60), 50.0),
        }],
        0,
        Code::Exists,
    );
    refused(
        &mut p,
        vec![Edit::AddKey {
            clip: c,
            key: fov_key(frame(301), 50.0),
        }],
        0,
        Code::OutOfRange,
    );
    let mut given = fov_key(frame(70), 50.0);
    if let AnyKey::Fov(k) = &mut given {
        k.id = KeyId(5);
    }
    refused(
        &mut p,
        vec![Edit::AddKey {
            clip: c,
            key: given,
        }],
        0,
        Code::IdGiven,
    );
    refused(
        &mut p,
        vec![Edit::AddKey {
            clip: ClipId(999),
            key: fov_key(frame(70), 50.0),
        }],
        0,
        Code::NotFound,
    );
    let spin = Edit::SetKey {
        key: first,
        at: None,
        value: None,
        ease: None,
        spin: Some(9),
    };
    refused(&mut p, vec![spin], 0, Code::OutOfRange);
    // A track keeps one key.
    let only = p.the_clip().view.projection.keys.clone();
    round_trip(&mut p, vec![Edit::RemoveKey { key: only[1].id }]);
    refused(
        &mut p,
        vec![Edit::RemoveKey { key: only[0].id }],
        0,
        Code::LastKey,
    );
}

#[test]
fn whole_tracks() {
    let mut p = project();
    let c = clip(&p);
    let old = p.the_clip().view.fov.keys.clone();
    let mut keys = old.clone();
    keys[0].value = 70.0;
    keys.push(Key {
        id: KeyId::NEW,
        at: frame(90),
        value: 40.0,
        ease: Ease::SMOOTH,
        spin: 0,
    });
    let after = round_trip(
        &mut p,
        vec![Edit::SetKeys {
            clip: c,
            keys: AnyKeys::Fov(keys.clone()),
        }],
    );
    let new = &after.the_clip().view.fov.keys;
    assert_eq!(new[0].id, old[0].id, "a key keeps its id");
    assert!(!new[1].id.is_new());
    refused(
        &mut p,
        vec![Edit::SetKeys {
            clip: c,
            keys: AnyKeys::Fov(vec![]),
        }],
        0,
        Code::LastKey,
    );
    let mut foreign = keys.clone();
    foreign[1].id = p.the_clip().view.orientation.keys[0].id;
    refused(
        &mut p,
        vec![Edit::SetKeys {
            clip: c,
            keys: AnyKeys::Fov(foreign),
        }],
        0,
        Code::IdGiven,
    );
    let mut backwards = keys.clone();
    backwards[1].at = Time::ZERO;
    refused(
        &mut p,
        vec![Edit::SetKeys {
            clip: c,
            keys: AnyKeys::Fov(backwards),
        }],
        0,
        Code::Exists,
    );
    round_trip(
        &mut p,
        vec![Edit::SetOrientMode {
            clip: c,
            mode: OrientMode::Free,
        }],
    );
    assert_eq!(p.the_clip().view.mode, OrientMode::Free);
}

#[test]
fn view_paths() {
    let mut p = project();
    let c = clip(&p);
    let point = |n, yaw, fov| PathPoint {
        at: frame(n),
        orientation: ypr(yaw, 0.0, 0.0),
        fov,
        orientation_key: KeyId::NEW,
        fov_key: KeyId::NEW,
    };
    let after = round_trip(
        &mut p,
        vec![Edit::SetViewPath {
            clip: c,
            points: vec![
                point(30, 10.0, None),
                point(60, 20.0, Some(80.0)),
                point(90, 30.0, None),
            ],
            replace: false,
        }],
    );
    let v = &after.the_clip().view;
    assert_eq!(v.orientation.keys.len(), 4);
    assert_eq!(v.fov.keys.len(), 2);
    // Without replace, a point on a key is refused; with it, the keys in
    // the span go.
    refused(
        &mut p,
        vec![Edit::SetViewPath {
            clip: c,
            points: vec![point(60, 0.0, None)],
            replace: false,
        }],
        0,
        Code::Exists,
    );
    let after = round_trip(
        &mut p,
        vec![Edit::SetViewPath {
            clip: c,
            points: vec![point(45, 0.0, None), point(75, 5.0, None)],
            replace: true,
        }],
    );
    let at: Vec<Time> = after
        .the_clip()
        .view
        .orientation
        .keys
        .iter()
        .map(|k| k.at)
        .collect();
    assert_eq!(at, [Time::ZERO, frame(30), frame(45), frame(75), frame(90)]);
    assert_eq!(
        after.the_clip().view.fov.keys.len(),
        2,
        "no fov in the points: fov keys stay"
    );
    refused(
        &mut p,
        vec![Edit::SetViewPath {
            clip: c,
            points: vec![],
            replace: true,
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::SetViewPath {
            clip: c,
            points: vec![point(50, 0.0, None), point(50, 1.0, None)],
            replace: true,
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::SetViewPath {
            clip: c,
            points: vec![point(50, 0.0, Some(400.0))],
            replace: true,
        }],
        0,
        Code::OutOfRange,
    );
}

#[test]
fn redo_makes_the_same_ids() {
    let mut p = project();
    let c = clip(&p);
    let mut h = History::new();
    let point = PathPoint {
        at: frame(30),
        orientation: ypr(10.0, 0.0, 0.0),
        fov: Some(50.0),
        orientation_key: KeyId::NEW,
        fov_key: KeyId::NEW,
    };
    h.record(
        &mut p,
        "Path",
        Origin::Agent {
            name: "test".into(),
        },
        &[Edit::SetViewPath {
            clip: c,
            points: vec![point],
            replace: false,
        }],
    )
    .unwrap();
    h.record(
        &mut p,
        "Marker",
        Origin::User,
        &[Edit::AddMarker {
            clip: c,
            marker: Marker {
                id: MarkerId::NEW,
                at: frame(3),
                name: "jump".into(),
            },
        }],
    )
    .unwrap();
    let done = p.clone();
    h.undo(&mut p).unwrap();
    h.undo(&mut p).unwrap();
    assert!(h.undo(&mut p).unwrap().is_none());
    h.redo(&mut p).unwrap();
    h.redo(&mut p).unwrap();
    assert!(h.redo(&mut p).unwrap().is_none());
    assert!(p.same_content(&done));
    assert_eq!(ids(&p), ids(&done));
}

#[test]
fn markers() {
    let mut p = project();
    let c = clip(&p);
    let marker = |n, name: &str| Marker {
        id: MarkerId::NEW,
        at: frame(n),
        name: name.into(),
    };
    let after = round_trip(
        &mut p,
        vec![
            Edit::AddMarker {
                clip: c,
                marker: marker(90, "b"),
            },
            Edit::AddMarker {
                clip: c,
                marker: marker(30, "a"),
            },
            Edit::AddMarker {
                clip: c,
                marker: marker(30, ""),
            },
        ],
    );
    let names: Vec<&str> = after
        .the_clip()
        .markers
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(names, ["a", "", "b"], "in time order, then id");
    let b = after.the_clip().markers[2].id;
    let after = round_trip(
        &mut p,
        vec![Edit::SetMarker {
            marker: b,
            at: Some(frame(0)),
            name: Some("start".into()),
        }],
    );
    assert_eq!(after.the_clip().markers[0].id, b);
    round_trip(
        &mut p,
        vec![Edit::Rename {
            id: b.0,
            name: "first".into(),
        }],
    );
    assert_eq!(p.the_clip().marker(b).unwrap().name, "first");
    round_trip(&mut p, vec![Edit::RemoveMarker { marker: b }]);
    let a = p.the_clip().markers[0].id;
    refused(
        &mut p,
        vec![Edit::SetMarker {
            marker: a,
            at: None,
            name: Some("two\nlines".into()),
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::SetMarker {
            marker: a,
            at: None,
            name: Some("x".repeat(201)),
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::SetMarker {
            marker: a,
            at: Some(frame(400)),
            name: None,
        }],
        0,
        Code::OutOfRange,
    );
    refused(
        &mut p,
        vec![Edit::SetMarker {
            marker: a,
            at: None,
            name: None,
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::RemoveMarker {
            marker: MarkerId(999),
        }],
        0,
        Code::NotFound,
    );
}

#[test]
fn targets() {
    let mut p = project();
    let c = clip(&p);
    let after = round_trip(
        &mut p,
        vec![Edit::AddTarget {
            target: target("rider", 30.0, -10.0),
        }],
    );
    let t = after.targets()[0].id;
    let aim = AnyKey::Orientation(Key {
        id: KeyId::NEW,
        at: frame(60),
        value: Orient::Aim {
            target: t,
            roll_deg: 0.0,
        },
        ease: Ease::SMOOTH,
        spin: 0,
    });
    round_trip(
        &mut p,
        vec![Edit::AddKey {
            clip: c,
            key: aim.clone(),
        }],
    );
    refused(
        &mut p,
        vec![Edit::RemoveTarget { target: t }],
        0,
        Code::InUse,
    );
    let mut path = p.targets()[0].path.clone();
    path.keys.push(Key {
        id: KeyId::NEW,
        at: frame(30),
        value: AimDir {
            yaw_deg: 40.0,
            pitch_deg: 0.0,
        },
        ease: Ease::SMOOTH,
        spin: 0,
    });
    round_trip(
        &mut p,
        vec![Edit::SetTarget {
            target: t,
            name: Some("bike".into()),
            path: Some(path.clone()),
        }],
    );
    round_trip(
        &mut p,
        vec![Edit::Rename {
            id: t.0,
            name: "the bike".into(),
        }],
    );
    let mut high = path.clone();
    high.keys[1].value.pitch_deg = 95.0;
    high.keys[1].id = KeyId::NEW;
    refused(
        &mut p,
        vec![Edit::SetTarget {
            target: t,
            name: None,
            path: Some(high),
        }],
        0,
        Code::OutOfRange,
    );
    refused(
        &mut p,
        vec![Edit::SetTarget {
            target: t,
            name: Some(String::new()),
            path: None,
        }],
        0,
        Code::Invalid,
    );
    let missing = AnyKey::Orientation(Key {
        id: KeyId::NEW,
        at: frame(90),
        value: Orient::Aim {
            target: TargetId(999),
            roll_deg: 0.0,
        },
        ease: Ease::SMOOTH,
        spin: 0,
    });
    refused(
        &mut p,
        vec![Edit::AddKey {
            clip: c,
            key: missing,
        }],
        0,
        Code::NotFound,
    );
    // Remove the aim key, then the target, in one step.
    let aim_key = p.the_clip().view.orientation.keys[1].id;
    round_trip(
        &mut p,
        vec![
            Edit::RemoveKey { key: aim_key },
            Edit::RemoveTarget { target: t },
        ],
    );
    assert!(p.targets().is_empty());
}

#[test]
fn rename() {
    let mut p = project();
    let s = sequence(&p);
    round_trip(
        &mut p,
        vec![Edit::Rename {
            id: s.0,
            name: "Beach run".into(),
        }],
    );
    assert_eq!(p.the_sequence().name, "Beach run");
    refused(
        &mut p,
        vec![Edit::Rename {
            id: s.0,
            name: String::new(),
        }],
        0,
        Code::Invalid,
    );
    refused(
        &mut p,
        vec![Edit::Rename {
            id: 999,
            name: "x".into(),
        }],
        0,
        Code::NotFound,
    );
    refused(&mut p, vec![], 0, Code::Invalid);
}

#[test]
fn every_op_is_named_as_serde_writes_it() {
    let mut p = project();
    let c = clip(&p);
    let s = sequence(&p);
    let samples = vec![
        Edit::AddMedia {
            media: common::media("/a.mp4", 1),
        },
        Edit::RemoveMedia { media: MediaId(1) },
        Edit::AddClip {
            track: TrackId(3),
            clip: p.the_clip().clone(),
        },
        Edit::RemoveClip { clip: c },
        Edit::SetTrim {
            clip: c,
            start: None,
            end: Some(frame(1)),
        },
        Edit::SetOutput {
            sequence: s,
            output: Output::DEFAULT,
        },
        Edit::SetRate {
            sequence: s,
            rate: spherecut_core::FrameRate::FPS_24,
        },
        Edit::SetExport {
            sequence: s,
            export: p.the_sequence().export,
        },
        Edit::AddKey {
            clip: c,
            key: fov_key(frame(1), 50.0),
        },
        Edit::SetKey {
            key: KeyId(5),
            at: None,
            value: None,
            ease: Some(Ease::STOP),
            spin: None,
        },
        Edit::RemoveKey { key: KeyId(5) },
        Edit::SetKeys {
            clip: c,
            keys: AnyKeys::Fov(vec![]),
        },
        Edit::SetOrientMode {
            clip: c,
            mode: OrientMode::Free,
        },
        Edit::SetViewPath {
            clip: c,
            points: vec![],
            replace: false,
        },
        Edit::AddMarker {
            clip: c,
            marker: Marker {
                id: MarkerId::NEW,
                at: frame(1),
                name: String::new(),
            },
        },
        Edit::SetMarker {
            marker: MarkerId(9),
            at: None,
            name: Some("x".into()),
        },
        Edit::RemoveMarker {
            marker: MarkerId(9),
        },
        Edit::AddTarget {
            target: target("t", 0.0, 0.0),
        },
        Edit::SetTarget {
            target: TargetId(9),
            name: Some("t".into()),
            path: None,
        },
        Edit::RemoveTarget {
            target: TargetId(9),
        },
        Edit::Rename {
            id: 2,
            name: "x".into(),
        },
    ];
    assert_eq!(samples.len(), Edit::OPS.len());
    for (e, op) in samples.iter().zip(Edit::OPS) {
        let json = serde_json::to_value(e).unwrap();
        assert_eq!(json["op"], op);
        assert_eq!(e.op(), op);
        let back: Edit = serde_json::from_value(json).unwrap();
        assert_eq!(&back, e);
    }
    // Unknown fields and ops are refused.
    for json in [
        r#"{"op": "set_trim", "clip": 4, "end": {"exact": "1"}, "ned": 3}"#,
        r#"{"op": "trim", "clip": 4}"#,
    ] {
        assert!(serde_json::from_str::<Edit>(json).is_err(), "{json}");
    }
    // An agent may leave ids and eases out.
    let e: Edit = serde_json::from_str(
        r#"{"op": "add_key", "clip": 4, "key": {"fov": {"at": {"exact": "1"}, "value": 60}}}"#,
    )
    .unwrap();
    p.apply(&[e]).unwrap();
}

#[test]
fn apply_is_fast_on_a_full_track() {
    let mut p = project();
    let c = clip(&p);
    let mut keys = Vec::new();
    for n in 0..10_000i64 {
        keys.push(Key {
            id: KeyId::NEW,
            at: Time(spherecut_core::Rational::new(n, 1001).unwrap()),
            value: 60.0 + (n % 30) as f64,
            ease: Ease::SMOOTH,
            spin: 0,
        });
    }
    p.apply(&[Edit::SetKeys {
        clip: c,
        keys: AnyKeys::Fov(keys),
    }])
    .unwrap();
    let key = p.the_clip().view.fov.keys[5000].id;
    let start = std::time::Instant::now();
    let n = 20;
    for i in 0..n {
        p.apply(&[Edit::SetKey {
            key,
            at: None,
            value: Some(AnyValue::Fov(50.0 + i as f64)),
            ease: None,
            spin: None,
        }])
        .unwrap();
    }
    let each = start.elapsed() / n;
    // 5 ms in the plan, for an optimised build; a debug build gets 10×.
    assert!(each.as_millis() < 50, "{each:?} per apply");
}
