//! Property tests of edits and undo (M2 plan section 4.4): random steps
//! of random edits, valid and invalid mixed, made against the project as
//! it is at each step.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::path::Path;

use common::*;
use proptest::prelude::*;
use spherecut_core::project::file;
use spherecut_core::project::*;
use spherecut_core::{FrameRate, Rational};
use spherecut_sphere::perspective::Projection;

/// One edit's worth of randomness, turned into an edit by [`make`]
/// against the project as it is when it is applied.
#[derive(Debug, Clone)]
struct Seed {
    op: u8,
    a: u64,
    b: u64,
    x: f64,
    y: f64,
    z: f64,
}

fn seed() -> impl Strategy<Value = Seed> {
    (
        any::<u8>(),
        any::<u64>(),
        any::<u64>(),
        -200.0f64..200.0,
        -100.0f64..100.0,
        -20.0f64..400.0,
    )
        .prop_map(|(op, a, b, x, y, z)| Seed { op, a, b, x, y, z })
}

/// A frame from 0 to 310 at 30 fps: a few past the 10 s clip's end, so
/// some times are refused.
fn time(n: u64) -> spherecut_core::Time {
    frame((n % 311) as i64)
}

fn pick<T: Copy>(items: &[T], n: u64) -> Option<T> {
    (!items.is_empty()).then(|| items[(n % items.len() as u64) as usize])
}

fn all_keys(p: &Project) -> Vec<KeyId> {
    let v = &p.the_clip().view;
    v.orientation
        .keys
        .iter()
        .map(|k| k.id)
        .chain(v.fov.keys.iter().map(|k| k.id))
        .chain(v.projection.keys.iter().map(|k| k.id))
        .collect()
}

fn ease(n: u64) -> Ease {
    let side = |n: u64| [Side::Smooth, Side::Linear, Side::Stop][(n % 3) as usize];
    Ease {
        into: side(n),
        out: side(n / 3),
        hold: n.is_multiple_of(7),
    }
}

fn make(p: &Project, s: &Seed) -> Edit {
    let c = clip(p);
    let seq = sequence(p);
    let keys = all_keys(p);
    let markers: Vec<MarkerId> = p.the_clip().markers.iter().map(|m| m.id).collect();
    let targets: Vec<TargetId> = p.targets().iter().map(|t| t.id).collect();
    let projection = [
        Projection::Rectilinear,
        Projection::Fisheye,
        Projection::Stereographic,
    ][(s.b % 3) as usize];
    let point = |n: u64, i: u64| PathPoint {
        at: time(n.wrapping_add(i * 7)),
        orientation: ypr(s.x + i as f64, s.y * 0.9, 0.0),
        fov: i.is_multiple_of(2).then_some(s.z.clamp(10.0, 360.0)),
        orientation_key: KeyId::NEW,
        fov_key: KeyId::NEW,
    };
    match s.op % 18 {
        0 => Edit::AddKey {
            clip: c,
            key: orientation_key(time(s.a), ypr(s.x, s.y * 0.9, s.z / 10.0)),
        },
        1 => Edit::AddKey {
            clip: c,
            key: fov_key(time(s.a), s.z),
        },
        2 => Edit::AddKey {
            clip: c,
            key: projection_key(time(s.a), projection),
        },
        3 => Edit::SetKey {
            key: pick(&keys, s.a).unwrap_or(KeyId(999)),
            at: s.b.is_multiple_of(2).then(|| time(s.b / 2)),
            value: None,
            ease: s.b.is_multiple_of(3).then(|| ease(s.a)),
            spin: s.b.is_multiple_of(5).then_some((s.y / 10.0) as i8),
        },
        4 => Edit::RemoveKey {
            key: pick(&keys, s.a).unwrap_or(KeyId(999)),
        },
        5 => Edit::SetTrim {
            clip: c,
            start: (s.a % 3 != 1).then(|| time(s.a / 3)),
            end: (s.a % 3 != 2).then(|| time(s.b)),
        },
        6 => Edit::AddMarker {
            clip: c,
            marker: Marker {
                id: MarkerId::NEW,
                at: time(s.a),
                name: format!("m{}", s.b % 100),
            },
        },
        7 => Edit::SetMarker {
            marker: pick(&markers, s.a).unwrap_or(MarkerId(999)),
            at: s.b.is_multiple_of(2).then(|| time(s.b)),
            name: s.b.is_multiple_of(3).then(|| {
                if s.b.is_multiple_of(9) {
                    "bad\n".into()
                } else {
                    "renamed".into()
                }
            }),
        },
        8 => Edit::RemoveMarker {
            marker: pick(&markers, s.a).unwrap_or(MarkerId(999)),
        },
        9 => Edit::AddTarget {
            target: target(&format!("t{}", s.a % 10), s.x, s.y),
        },
        10 => match pick(&targets, s.a) {
            Some(t) => Edit::RemoveTarget { target: t },
            None => Edit::RemoveTarget {
                target: TargetId(999),
            },
        },
        11 => Edit::AddKey {
            clip: c,
            key: AnyKey::Orientation(Key {
                id: KeyId::NEW,
                at: time(s.b),
                value: Orient::Aim {
                    target: pick(&targets, s.a).unwrap_or(TargetId(999)),
                    roll_deg: s.y,
                },
                ease: ease(s.b),
                spin: (s.a % 4) as i8,
            }),
        },
        12 => Edit::SetViewPath {
            clip: c,
            points: (0..1 + s.b % 4).map(|i| point(s.a, i)).collect(),
            replace: s.b.is_multiple_of(2),
        },
        13 => Edit::SetOrientMode {
            clip: c,
            mode: if s.a.is_multiple_of(2) {
                OrientMode::Level
            } else {
                OrientMode::Free
            },
        },
        14 => Edit::SetOutput {
            sequence: seq,
            output: if s.a.is_multiple_of(2) {
                Output::Flat {
                    aspect: Aspect::ALL[(s.b % 4) as usize],
                    short_side: [1080, 1440, 2160, 1000][(s.b / 4 % 4) as usize],
                }
            } else {
                Output::Equirect {
                    width: [3840, 5760, 7680, 4096][(s.b % 4) as usize],
                    orientation: ypr(s.x, 0.0, 0.0),
                }
            },
        },
        15 => Edit::SetRate {
            sequence: seq,
            rate: FrameRate::new(
                Rational::new(1 + (s.a % 300) as i64, 1 + (s.b % 2) as i64 * 1000).unwrap(),
            )
            .unwrap(),
        },
        16 => Edit::Rename {
            id: pick(
                &[
                    seq.0,
                    markers.first().map_or(999, |m| m.0),
                    targets.first().map_or(998, |t| t.0),
                ],
                s.a,
            )
            .unwrap_or(999),
            name: if s.b.is_multiple_of(5) {
                String::new()
            } else {
                format!("n{}", s.b % 1000)
            },
        },
        _ => {
            // The fov track replaced by a shuffled copy, a new key added.
            let mut keys = p.the_clip().view.fov.keys.clone();
            if s.a.is_multiple_of(3) {
                keys.reverse();
            }
            keys.push(Key {
                id: KeyId::NEW,
                at: time(s.b),
                value: s.z,
                ease: ease(s.a),
                spin: 0,
            });
            keys.sort_by_key(|k| k.at);
            Edit::SetKeys {
                clip: c,
                keys: AnyKeys::Fov(keys),
            }
        }
    }
}

/// Check that no id ever names two kinds of thing over a run.
fn note_ids(p: &Project, seen: &mut HashMap<u64, &'static str>) -> Result<(), TestCaseError> {
    for (id, kind) in ids(p) {
        let was = *seen.entry(id).or_insert(kind);
        prop_assert_eq!(was, kind, "id {} named a {} and now a {}", id, was, kind);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)
    ))]

    #[test]
    fn edits_undo_redo_and_save(steps in prop::collection::vec((seed(), seed(), any::<bool>()), 1..40)) {
        let mut p = project();
        let original = p.clone();
        let mut h = History::new();
        let mut seen = HashMap::new();
        let mut states = vec![p.clone()];
        let mut last_next_id = p.next_id();
        for (a, b, two) in &steps {
            let mut edits = vec![make(&p, a)];
            if *two {
                edits.push(make(&p, b));
            }
            let before = p.clone();
            match h.record(&mut p, "step", Origin::User, &edits) {
                Ok(step) => {
                    prop_assert_eq!(step.before, before.revision());
                    prop_assert_eq!(step.after, before.revision() + 1);
                    prop_assert!(p.validate().is_ok());
                    states.push(p.clone());
                }
                Err(e) => {
                    prop_assert!(e.index < edits.len(), "{}", e);
                    prop_assert_eq!(&p, &before, "a rejected edit changes nothing: {}", e);
                }
            }
            prop_assert!(p.next_id() >= last_next_id);
            last_next_id = p.next_id();
            note_ids(&p, &mut seen)?;
        }
        // Save then load gives an equal project.
        let text = file::to_json(&p, Path::new("/clips")).unwrap();
        prop_assert_eq!(&file::from_json(&text, Path::new("/clips")).unwrap(), &p);
        // Undo everything: each state comes back in turn, then the original.
        let done = states.len() - 1;
        for i in (0..done).rev() {
            let rev = p.revision();
            prop_assert!(h.undo(&mut p).unwrap().is_some());
            prop_assert_eq!(p.revision(), rev + 1);
            prop_assert!(p.same_content(&states[i]));
            note_ids(&p, &mut seen)?;
        }
        prop_assert!(h.undo(&mut p).unwrap().is_none());
        prop_assert!(p.same_content(&original));
        // Redo everything: the same states, with the same ids.
        for state in &states[1..] {
            prop_assert!(h.redo(&mut p).unwrap().is_some());
            prop_assert!(p.same_content(state));
            prop_assert_eq!(ids(&p), ids(state));
        }
        prop_assert!(h.redo(&mut p).unwrap().is_none());
        // Undo, then redo, gives the same project as before the undo.
        if done > 0 {
            let top = p.clone();
            h.undo(&mut p).unwrap();
            h.redo(&mut p).unwrap();
            prop_assert!(p.same_content(&top));
        }
    }

    /// A gesture shows each frame's edits and ends as one step.
    #[test]
    fn gestures_are_one_step(yaws in prop::collection::vec(-180.0f64..180.0, 1..30), cancel in any::<bool>()) {
        let mut p = project();
        let before = p.clone();
        let key = p.the_clip().view.orientation.keys[0].id;
        let mut h = History::new();
        let g = Gesture::start(&p, "Aim", Origin::User);
        let edit = |yaw: f64| vec![Edit::SetKey {
            key,
            at: None,
            value: Some(AnyValue::Orientation(Orient::View(ypr(yaw, 0.0, 0.0)))),
            ease: None,
            spin: None,
        }];
        for &yaw in &yaws {
            g.update(&mut p, &edit(yaw)).unwrap();
            prop_assert_eq!(p.revision(), before.revision() + 1);
        }
        let last = edit(*yaws.last().unwrap());
        if cancel {
            g.cancel(&mut p);
            prop_assert_eq!(&p, &before);
            prop_assert_eq!(h.steps().len(), 0);
        } else {
            h.finish(g, &mut p, &last).unwrap();
            prop_assert_eq!(h.steps().len(), 1);
            prop_assert_eq!(p.revision(), before.revision() + 1);
            h.undo(&mut p).unwrap();
            prop_assert!(p.same_content(&before));
        }
    }
}

#[test]
fn history_keeps_its_limit_and_drops_redo_on_a_new_step() {
    let mut p = project();
    let c = clip(&p);
    let mut h = History::with_limit(3);
    for n in 1..=5 {
        h.record(
            &mut p,
            format!("Marker {n}"),
            Origin::User,
            &[Edit::AddMarker {
                clip: c,
                marker: Marker {
                    id: MarkerId::NEW,
                    at: frame(n),
                    name: String::new(),
                },
            }],
        )
        .unwrap();
    }
    assert_eq!(h.steps().len(), 3);
    assert_eq!(h.steps()[0].label, "Marker 3");
    h.undo(&mut p).unwrap();
    assert_eq!(h.redo_step().unwrap().label, "Marker 5");
    assert_eq!(h.undo_step().unwrap().label, "Marker 4");
    h.record(
        &mut p,
        "Other",
        Origin::Agent {
            name: "claude".into(),
        },
        &[Edit::SetOrientMode {
            clip: c,
            mode: OrientMode::Free,
        }],
    )
    .unwrap();
    assert!(h.redo_step().is_none(), "a new step drops the undone ones");
    assert_eq!(h.steps().len(), 3);
    let step = h.undo_step().unwrap();
    assert_eq!(
        step.origin,
        Origin::Agent {
            name: "claude".into()
        }
    );
    let json = serde_json::to_value(step).unwrap();
    assert_eq!(
        json["origin"],
        serde_json::json!({"kind": "agent", "name": "claude"})
    );
    let back: Step = serde_json::from_value(json).unwrap();
    assert_eq!(&back, step);
}
