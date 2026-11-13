//! Every `Edit` variant applied, undone, redone and refused (M2 plan
//! sections 4 and 11).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use spherecut_core::Time;
use spherecut_core::project::*;
use spherecut_sphere::fisheye::V6Reading;
use spherecut_sphere::perspective::{LensShape, Projection};
use spherecut_sphere::rig::{
    Binding, CalibTerms, CanvasMap, EulerConv, Extrinsics, Generation, Geometry, LensCalib,
    QuarterTurn, Status, Tune, TurnAxis, TzSign,
};
use spherecut_testkit::control::must_fail;

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
        vec![set_value(
            fov,
            AnyValue::Projection(LensShape::Pure(Projection::Fisheye)),
        )],
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
fn targets_belong_to_their_media() {
    let mut p = project();
    let c = clip(&p);
    p.apply(&[Edit::AddMedia {
        media: common::media("/clips/VID_2.mp4", 5),
    }])
    .unwrap();
    let other = p.media()[1].id;
    let mut there = target(&p, "elsewhere", 0.0, 0.0);
    there.media = other;
    // A target's path is in its media's time, so it ends with the media.
    let mut long = there.clone();
    long.path.keys[0].at = frame(200);
    refused(
        &mut p,
        vec![Edit::AddTarget { target: long }],
        0,
        Code::OutOfRange,
    );
    // Only orientation keys spin; a path key's spin would be kept and
    // never used.
    let mut spun = there.clone();
    spun.path.keys[0].spin = 1;
    refused(
        &mut p,
        vec![Edit::AddTarget { target: spun }],
        0,
        Code::Invalid,
    );
    let mut lost = there.clone();
    lost.media = MediaId(77);
    refused(
        &mut p,
        vec![Edit::AddTarget { target: lost }],
        0,
        Code::NotFound,
    );
    p.apply(&[Edit::AddTarget { target: there }]).unwrap();
    let t = p.targets()[0].id;
    // The clip's media isn't the target's.
    let aim = AnyKey::Orientation(Key {
        id: KeyId::NEW,
        at: frame(30),
        value: Orient::Aim {
            target: t,
            roll_deg: 0.0,
        },
        ease: Ease::SMOOTH,
        spin: 0,
    });
    let e = refused(
        &mut p,
        vec![Edit::AddKey { clip: c, key: aim }],
        0,
        Code::Invalid,
    );
    assert!(e.to_string().contains("not the clip's"), "{e}");
    // Media a target is in stays while the target does.
    refused(
        &mut p,
        vec![Edit::RemoveMedia { media: other }],
        0,
        Code::InUse,
    );
    round_trip(
        &mut p,
        vec![
            Edit::RemoveTarget { target: t },
            Edit::RemoveMedia { media: other },
        ],
    );
}

#[test]
fn targets() {
    let mut p = project();
    let c = clip(&p);
    let rider = target(&p, "rider", 30.0, -10.0);
    let after = round_trip(&mut p, vec![Edit::AddTarget { target: rider }]);
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
            target: target(&project(), "t", 0.0, 0.0),
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
        Edit::SetSource {
            media: MediaId(1),
            source: Source::default(),
        },
        Edit::SetStitch {
            media: MediaId(1),
            tune: Some(Tune::default()),
            gains: Some(Gains::Off {}),
            profile: None,
        },
        Edit::SetCaps {
            media: MediaId(1),
            caps: Caps::default(),
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
    let bound = if cfg!(debug_assertions) { 50 } else { 5 };
    assert!(each.as_millis() < bound, "{each:?} per apply");
}

#[test]
fn keys_too_close_for_seconds_are_refused() {
    let mut p = project();
    let c = clip(&p);
    // Apart as fractions, one time in seconds: the spline would divide
    // by nothing between them.
    let one = Time(spherecut_core::Rational::new(1, 1).unwrap());
    let next =
        Time(spherecut_core::Rational::new(9_007_199_254_740_993, 9_007_199_254_740_992).unwrap());
    assert!(next > one && next.to_secs_f64() == one.to_secs_f64());
    p.apply(&[Edit::AddKey {
        clip: c,
        key: fov_key(one, 60.0),
    }])
    .unwrap();
    refused(
        &mut p,
        vec![Edit::AddKey {
            clip: c,
            key: fov_key(next, 60.0),
        }],
        0,
        Code::Exists,
    );
}

/// A camera file's two-lens source in the X5's shape (M3 plan 11.1),
/// under the X5 row's conventions with a calibration of my own: synth's
/// `x5_like_v6` numbers with their last digits changed and its distortion
/// cut to the radial terms, never Pat's (F137). A tune, gains and caps
/// edit only what it holds.
fn x5_shaped() -> DualFisheye {
    let block = |fx: f64, fy: f64, cx: f64, cy: f64, ypr_deg, t_m, d: [f64; 4]| LensCalib {
        xi: 2.0,
        fx,
        fy,
        cx,
        cy,
        ypr_deg,
        t_m,
        terms: CalibTerms::V6 {
            d: std::array::from_fn(|i| d.get(i).copied().unwrap_or(0.0)),
        },
    };
    let raw = RawStrings {
        v1: None,
        v2: None,
        v3: None,
        v6: Some("a V6 string of my own".into()),
    };
    DualFisheye {
        camera: Camera {
            make: Some("Insta360".into()),
            model: Some("Insta360 X5".into()),
            firmware: Some("v1.1.22_build1".into()),
            serial: Some("SYNTH00001".into()),
        },
        tracks: [0, 1],
        front_track: 0,
        calibration: CalibSnapshot {
            canvas: [10752, 5376],
            window: [5312, 5312],
            window_offset: [0, 0],
            hash: raw.hash(),
            raw,
            v3: None,
            v6: Some([
                block(
                    4277.56,
                    4277.23,
                    2704.7,
                    2673.4,
                    [0.31, -0.22, 90.47],
                    [0.0; 3],
                    [0.28032991, 0.89045402, 2.04927581, -8.05179784],
                ),
                block(
                    4284.44,
                    4284.72,
                    2669.2,
                    2703.3,
                    [-0.26, 0.33, 89.32],
                    [-0.00447, 0.000018, -0.0322],
                    [0.27665338, 0.98522977, 1.83605261, -7.99374482],
                ),
            ]),
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
        tune: Tune::default(),
        gains: Gains::Off {},
        profile: None,
    }
}

/// A new project on a 10 s camera file of two 3840² lenses whose source
/// is [`x5_shaped`].
fn camera_project() -> Project {
    let mut m = common::media("/clips/VID_20250101_120000_00_001.insv", 10);
    m.video.width = 3840;
    m.video.height = 3840;
    m.source = Source::DualFisheye(Box::new(x5_shaped()));
    Project::untitled(
        m,
        InitialView {
            orientation: ypr(20.0, -5.0, 0.0),
            fov_deg: 90.0,
            projection: Projection::Rectilinear,
        },
    )
    .unwrap()
}

/// The dual-fisheye source of media `id` in `p`.
fn fisheye(p: &Project, id: MediaId) -> &DualFisheye {
    match &p.media_by_id(id).unwrap().source {
        Source::DualFisheye(d) => d,
        other => panic!("media {id} is {other:?}"),
    }
}

/// 11.5's tune: q = 1/1.5 m⁻¹, lens B yawed 0.3° and a 3° feather.
fn fixture_tune() -> Tune {
    let mut tune = Tune {
        feather_deg: 3.0,
        q_per_m: 1.0 / 1.5,
        ..Tune::default()
    };
    tune.lenses[1].ypr_deg = [0.3, 0.0, 0.0];
    tune
}

/// Fixed gains of `g` on lens B.
fn lens_b_gains(g: f64) -> Gains {
    Gains::Fixed {
        rgb: [[1.0; 3], [g; 3]],
    }
}

/// Whether `inverse`, applied to `after`, gives a project with `before`'s
/// content: the check that an undo is exact.
fn undoes(after: &Project, inverse: &[Edit], before: &Project) -> bool {
    let mut p = after.clone();
    p.apply(inverse).is_ok() && p.same_content(before)
}

/// `set_source`, `set_stitch` and `set_caps` on a dual-fisheye media
/// (M3 plan 11.3): each change, undone to an equal project and redone to
/// the edited one, an agent's step keeping its name. Control: an inverse
/// made of the new value fails the undo.
#[test]
fn the_stitch_ops_apply_and_undo_exactly() {
    let mut p = camera_project();
    let m = p.the_clip().media;
    let first = x5_shaped();

    // set_source: the binding swapped by hand, as status User says, then
    // the file tagged as equirect and stitched again.
    let mut swapped = x5_shaped();
    swapped.geometry.binding = Binding::Direct;
    swapped.geometry.status = Status::User;
    let after = round_trip(
        &mut p,
        vec![Edit::SetSource {
            media: m,
            source: Source::DualFisheye(Box::new(swapped.clone())),
        }],
    );
    assert_eq!(fisheye(&after, m), &swapped);
    assert_eq!(fisheye(&p, m), &swapped, "redo leaves the edit in place");
    let after = round_trip(
        &mut p,
        vec![Edit::SetSource {
            media: m,
            source: Source::Equirect {},
        }],
    );
    assert_eq!(after.media_by_id(m).unwrap().source, Source::Equirect {});
    round_trip(
        &mut p,
        vec![Edit::SetSource {
            media: m,
            source: Source::DualFisheye(Box::new(first.clone())),
        }],
    );
    assert_eq!(fisheye(&p, m), &first);

    // set_stitch: a tune from a profile, then gains alone, which keep the
    // tune and its profile, then a tune with no profile, which clears it,
    // then both at once.
    let profile = ProfileRef::of("tripod", br#"{"name": "tripod"}"#);
    let after = round_trip(
        &mut p,
        vec![Edit::SetStitch {
            media: m,
            tune: Some(fixture_tune()),
            gains: None,
            profile: Some(profile.clone()),
        }],
    );
    let d = fisheye(&after, m);
    assert_eq!(
        (&d.tune, &d.gains, &d.profile),
        (&fixture_tune(), &Gains::Off {}, &Some(profile.clone()))
    );
    let after = round_trip(
        &mut p,
        vec![Edit::SetStitch {
            media: m,
            tune: None,
            gains: Some(lens_b_gains(0.9)),
            profile: None,
        }],
    );
    let d = fisheye(&after, m);
    assert_eq!(
        (&d.tune, &d.gains, &d.profile),
        (&fixture_tune(), &lens_b_gains(0.9), &Some(profile.clone()))
    );
    let mut wider = fixture_tune();
    wider.feather_deg = 2.0;
    let after = round_trip(
        &mut p,
        vec![Edit::SetStitch {
            media: m,
            tune: Some(wider),
            gains: None,
            profile: None,
        }],
    );
    let d = fisheye(&after, m);
    assert_eq!(
        (&d.tune, &d.gains, &d.profile),
        (&wider, &lens_b_gains(0.9), &None)
    );
    let after = round_trip(
        &mut p,
        vec![Edit::SetStitch {
            media: m,
            tune: Some(Tune::default()),
            gains: Some(Gains::Off {}),
            profile: Some(profile.clone()),
        }],
    );
    let d = fisheye(&after, m);
    assert_eq!(
        (&d.tune, &d.gains, &d.profile),
        (&Tune::default(), &Gains::Off {}, &Some(profile.clone()))
    );
    // Nothing but the stitch changed: the calibration and geometry are
    // the source's.
    assert_eq!(
        (&d.calibration, &d.geometry, d.tracks),
        (&first.calibration, &first.geometry, first.tracks)
    );

    // set_caps: a nadir fill, the zenith left bare.
    let caps = Caps {
        nadir: Some(Box::new(Cap {
            mode: CapMode::Fill { ring_deg: 2.0 },
            cap_deg: 20.0,
            edge_deg: 2.5,
            rotation_deg: 0.0,
        })),
        zenith: None,
    };
    let after = round_trip(
        &mut p,
        vec![Edit::SetCaps {
            media: m,
            caps: caps.clone(),
        }],
    );
    assert_eq!(after.media_by_id(m).unwrap().caps, caps);

    // An agent's step, by its name: undo gives the project before it,
    // redo the one after, and the step keeps its label and origin.
    let mut h = History::new();
    let before = p.clone();
    let agent = Origin::Agent {
        name: "claude".into(),
    };
    let edit = Edit::SetStitch {
        media: m,
        tune: Some(fixture_tune()),
        gains: Some(lens_b_gains(0.9)),
        profile: None,
    };
    let step = h
        .record(&mut p, "Stitch", agent.clone(), std::slice::from_ref(&edit))
        .unwrap()
        .clone();
    assert_eq!((step.label.as_str(), &step.origin), ("Stitch", &agent));
    let after = p.clone();
    let inverse = step.inverse.clone();
    assert!(undoes(&after, &inverse, &before));
    let undone = h.undo(&mut p).unwrap().unwrap();
    assert_eq!((undone.label.as_str(), &undone.origin), ("Stitch", &agent));
    assert!(p.same_content(&before));
    h.redo(&mut p).unwrap().unwrap();
    assert!(p.same_content(&after));

    // Control: an inverse made of the new value leaves the edit in place.
    // Each edit changes what `before` holds: its caps are the nadir fill.
    for (name, edit) in [
        ("set_stitch", edit),
        (
            "set_source",
            Edit::SetSource {
                media: m,
                source: Source::Equirect {},
            },
        ),
        (
            "set_caps",
            Edit::SetCaps {
                media: m,
                caps: Caps::default(),
            },
        ),
    ] {
        let mut q = before.clone();
        q.apply(std::slice::from_ref(&edit)).unwrap();
        must_fail(
            &format!("{name}'s inverse made of its new value undoes it"),
            undoes(&q, &[edit], &before),
        );
    }
}

/// Whether `edits` are refused with `code` and leave `p` as it was: the
/// check [`refused`] makes, as a value a control can hand to `must_fail`.
fn refuses(p: &Project, edits: &[Edit], code: Code) -> Result<(), String> {
    let mut q = p.clone();
    match q.apply(edits) {
        Ok(_) => Err(format!("{edits:?} applied")),
        Err(e) if e.code() != code => Err(format!("{e}: {} for {code}", e.code())),
        Err(_) if q != *p => Err("a refusal changed the project".into()),
        Err(_) => Ok(()),
    }
}

/// `set_stitch` with neither a tune nor gains, a profile without a tune,
/// on an equirect media, or on a media that isn't there is refused by its
/// code, as is each stitch op on a media that isn't there. Control: gains
/// alone apply.
#[test]
fn set_stitch_needs_a_tune_or_gains() {
    let mut p = camera_project();
    let m = p.the_clip().media;
    let stitch = |media, tune, gains, profile| Edit::SetStitch {
        media,
        tune,
        gains,
        profile,
    };
    let tripod = || Some(ProfileRef::of("tripod", b"{}"));
    for edit in [
        stitch(m, None, None, None),
        stitch(m, None, None, tripod()),
        stitch(m, None, Some(lens_b_gains(0.9)), tripod()),
    ] {
        refuses(&p, &[edit], Code::Invalid).unwrap();
    }
    // An equirect media has no stitch, whatever the edit holds.
    let mut plain = project();
    let flat = plain.the_clip().media;
    for edit in [
        stitch(flat, Some(fixture_tune()), None, None),
        stitch(flat, None, Some(lens_b_gains(0.9)), None),
    ] {
        refuses(&plain, &[edit], Code::Invalid).unwrap();
    }
    let gone = MediaId(999);
    for edit in [
        stitch(gone, Some(fixture_tune()), None, None),
        Edit::SetSource {
            media: gone,
            source: Source::Equirect {},
        },
        Edit::SetCaps {
            media: gone,
            caps: Caps::default(),
        },
    ] {
        refuses(&p, &[edit], Code::NotFound).unwrap();
    }
    // As a list: the refused edit is named, and nothing applies.
    let e = refused(
        &mut p,
        vec![
            stitch(m, Some(fixture_tune()), None, None),
            stitch(m, None, None, None),
        ],
        1,
        Code::Invalid,
    );
    assert_eq!(e.op, "set_stitch");
    // Caps are any source's.
    let caps = Caps {
        nadir: None,
        zenith: Some(Box::new(Cap {
            mode: CapMode::Fill { ring_deg: 1.0 },
            cap_deg: 10.0,
            edge_deg: 0.0,
            rotation_deg: 0.0,
        })),
    };
    let after = round_trip(
        &mut plain,
        vec![Edit::SetCaps {
            media: flat,
            caps: caps.clone(),
        }],
    );
    assert_eq!(after.media_by_id(flat).unwrap().caps, caps);

    // Control: gains alone apply.
    must_fail(
        "set_stitch with gains alone is refused",
        refuses(
            &p,
            &[stitch(m, None, Some(lens_b_gains(0.9)), None)],
            Code::Invalid,
        ),
    );
}

/// `set_stitch` on a camera's EAC pair is refused by the media's kind, as
/// on an equirect media: an EAC pair has no tune or gains (the M3.9
/// review's N5). Control: the same edit on the same media as a fisheye
/// pair applies.
#[test]
fn set_stitch_on_an_eac_pair_is_refused() {
    let fisheyes = camera_project();
    let m = fisheyes.the_clip().media;
    let mut pair = fisheyes.clone();
    pair.apply(&[Edit::SetSource {
        media: m,
        source: Source::EacPair(Box::new(EacPair {
            camera: x5_shaped().camera,
            tracks: [0, 1],
            layout: EacLayout {
                overlap: 64,
                pre: 1152,
                post: 1024,
                size: [4096, 1344],
            },
        })),
    }])
    .unwrap();
    let stitch = || Edit::SetStitch {
        media: m,
        tune: Some(fixture_tune()),
        gains: Some(lens_b_gains(0.9)),
        profile: None,
    };
    refuses(&pair, &[stitch()], Code::Invalid).unwrap();
    let e = pair.clone().apply(&[stitch()]).unwrap_err();
    assert!(
        e.to_string()
            .contains("is a camera's two EAC tracks, not a camera's two fisheye lenses"),
        "{e}"
    );

    must_fail(
        "set_stitch on the same media as a fisheye pair refused",
        refuses(&fisheyes, &[stitch()], Code::Invalid),
    );
}

/// Whether `History::record` refuses `edits` with `code` and leaves both
/// `p` and `h` as they were, a step undone above the cursor included.
fn record_refuses(p: &Project, h: &History, edits: &[Edit], code: Code) -> Result<(), String> {
    let (mut q, mut g) = (p.clone(), h.clone());
    let recorded = g.record(&mut q, "Stitch", Origin::User, edits).map(drop);
    match recorded {
        Ok(_) => Err(format!("{edits:?} recorded")),
        Err(e) if e.code() != code => Err(format!("{e}: {} for {code}", e.code())),
        Err(_) if q != *p => Err("a refusal changed the project".into()),
        Err(_) if g != *h => Err("a refusal changed the history".into()),
        Err(_) => Ok(()),
    }
}

/// A stitch op that `set_stitch`'s own checks pass and `validate`
/// refuses, a tune past its bounds, leaves the project and the history
/// as they were: no revision, no step, and the undone step above the
/// cursor kept for redo (the M3.9 review's N5). Control: the same edit
/// with its tune inside the bounds is recorded.
#[test]
fn a_stitch_validate_refuses_leaves_project_and_history() {
    let mut p = camera_project();
    let m = p.the_clip().media;
    let stitch = |feather_deg| {
        let mut tune = fixture_tune();
        tune.feather_deg = feather_deg;
        Edit::SetStitch {
            media: m,
            tune: Some(tune),
            gains: None,
            profile: None,
        }
    };
    // Two steps, one undone, so a refusal that dropped the redo would show.
    let mut h = History::new();
    h.record(&mut p, "Stitch", Origin::User, &[stitch(2.0)])
        .unwrap();
    h.record(&mut p, "Stitch", Origin::User, &[stitch(4.0)])
        .unwrap();
    h.undo(&mut p).unwrap().unwrap();
    assert_eq!((h.steps().len(), h.cursor()), (2, 1));

    // Past the feather's 0° to 15° (validate's `FEATHER_DEG`).
    let past = 16.0;
    record_refuses(&p, &h, &[stitch(past)], Code::OutOfRange).unwrap();
    let e = p.clone().apply(&[stitch(past)]).unwrap_err();
    assert_eq!((e.index, e.op), (0, "set_stitch"), "{e}");
    assert!(e.to_string().contains("source.tune.feather_deg"), "{e}");

    let recorded = must_fail(
        "the same stitch with its tune inside the bounds refused",
        record_refuses(&p, &h, &[stitch(3.0)], Code::OutOfRange),
    );
    assert!(recorded.ends_with("recorded"), "{recorded}");
}
