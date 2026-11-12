//! Property tests of edits and undo (M2 plan section 4.4): random steps
//! of random edits, valid and invalid mixed, made against the project as
//! it is at each step.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::path::Path;

use common::*;
use glam::DVec3;
use proptest::prelude::*;
use spherecut_core::project::file;
use spherecut_core::project::*;
use spherecut_core::{FrameRate, Rational};
use spherecut_sphere::fisheye::V6Reading;
use spherecut_sphere::perspective::{LensShape, Morph, Projection};
use spherecut_sphere::rig::{
    Binding, CalibTerms, CanvasMap, EulerConv, Extrinsics, Generation, Geometry, LensCalib,
    LensTune, QuarterTurn, Rig, RigLens, Status, Tune, TurnAxis, TzSign,
};
use spherecut_testkit::control::{at_least, must_fail};

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
    let media: Vec<MediaId> = p.media().iter().map(|m| m.id).collect();
    match s.op % 22 {
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
            key: if s.b.is_multiple_of(2) {
                projection_key(time(s.a), projection)
            } else {
                // A morph, sometimes outside 0 to 1 or to itself.
                AnyKey::Projection(Key {
                    id: KeyId::NEW,
                    at: time(s.a),
                    value: LensShape::Morph(Morph {
                        from: projection,
                        to: [Projection::Fisheye, Projection::Stereographic][(s.a % 2) as usize],
                        morph: s.z / 380.0,
                    }),
                    ease: Ease::STOP,
                    spin: 0,
                })
            },
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
            target: target(p, &format!("t{}", s.a % 10), s.x, s.y),
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
        18 => Edit::SetTarget {
            target: pick(&targets, s.a).unwrap_or(TargetId(999)),
            name: s.b.is_multiple_of(2).then(|| format!("r{}", s.b % 10)),
            path: s.b.is_multiple_of(3).then(|| {
                // Sometimes past the media's end, which is refused.
                let mut path = target(p, "x", s.y, s.y * 0.5).path;
                path.keys.push(Key {
                    id: KeyId::NEW,
                    at: time(s.a),
                    value: AimDir {
                        yaw_deg: s.x,
                        pitch_deg: (s.z / 4.0).clamp(-90.0, 90.0),
                    },
                    ease: ease(s.b),
                    spin: 0,
                });
                path.keys.sort_by_key(|k| k.at);
                path
            }),
        },
        19 => Edit::AddMedia {
            media: common::media(
                &format!("/clips/more_{}.mp4", s.a % 5),
                1 + (s.b % 20) as i64,
            ),
        },
        20 => Edit::RemoveMedia {
            // The clip's own is in use, and refused.
            media: pick(&media, s.a).unwrap_or(MediaId(999)),
        },
        21 => Edit::SetExport {
            sequence: seq,
            export: ExportSettings {
                codec: [Codec::H264, Codec::Hevc, Codec::Av1][(s.a % 3) as usize],
                quality: [Quality::High, Quality::Medium, Quality::Small][(s.b % 3) as usize],
                encoder: [
                    EncoderChoice::Auto,
                    EncoderChoice::Vaapi,
                    EncoderChoice::Software,
                ][(s.a / 3 % 3) as usize],
                audio: [AudioChoice::Copy, AudioChoice::Aac, AudioChoice::None]
                    [(s.b / 3 % 3) as usize],
                motion_blur: s.a.is_multiple_of(2),
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
    // A2 saves the whole history and reads it back.
    let saved = serde_json::to_string(&h).unwrap();
    let read: History = serde_json::from_str(&saved).unwrap();
    assert_eq!(read, h);
    let mut bad: serde_json::Value = serde_json::from_str(&saved).unwrap();
    bad["cursor"] = serde_json::json!(9);
    let e = serde_json::from_value::<History>(bad)
        .unwrap_err()
        .to_string();
    assert!(e.contains("cursor 9 is past its 3 steps"), "{e}");
}

/// The bands the stitch property draws each lens's tune from: inside
/// 11.2's bounds, and narrowed until every corner of them clears the
/// frame and leaves no direction uncovered ([`the_drawn_tunes_clear_the_frame`]),
/// which `validate` holds each tune to (11.2). On the base calibration a
/// radius scale of 1.05 leaves the frame 0.74° past 90° at θ_max 95°,
/// under the 3° feather, and ±1° of turn at q 3.34 leaves −0.22° of
/// coverage, so the radius scale is held to ±1% and the turn to ±0.5°;
/// q keeps 11.2's whole range.
mod band {
    /// Each lens's yaw, pitch and roll, ± degrees (11.2: ±15).
    pub const YPR_DEG: f64 = 0.5;
    /// Each lens's centre shift, ± canvas pixels (11.2: ±64).
    pub const CENTRE_PX: f64 = 16.0;
    /// The radius scale's least and most (11.2: 0.9 to 1.1).
    pub const RADIUS_SCALE: [f64; 2] = [0.99, 1.01];
    /// ξ's delta, ± (11.2 gives no range; F038 keeps ξ + it above −1).
    pub const XI_DELTA: f64 = 0.05;
    /// θ_max's least and most, degrees (11.2: 85 to 110).
    pub const THETA_MAX_DEG: [f64; 2] = [95.0, 105.0];
    /// The feather's most, degrees (11.2: 0 to 15).
    pub const FEATHER_DEG: f64 = 3.0;
    /// q's most, m⁻¹ (11.2: 0 to 3.34).
    pub const Q_PER_M: f64 = 3.34;
}

/// The base calibration of the stitch property's camera source, in the
/// X5's shape with numbers of my own (F137: none of Pat's): synth's
/// `x5_like` V3 blocks and `x5_like_v6` V6 blocks with their last digits
/// changed, both held as a file holding both strings holds them, under
/// the X5 row's conventions with `generation` in use.
fn base_source(generation: Generation) -> DualFisheye {
    let block = |b: usize, fx: f64, fy: f64, terms| {
        let ([cx, cy], ypr_deg, t_m) = [
            ([2704.7, 2673.4], [0.31, -0.22, 90.47], [0.0; 3]),
            (
                [2669.2, 2703.3],
                [-0.26, 0.33, 89.32],
                [-0.00447, 0.000018, -0.0322],
            ),
        ][b];
        LensCalib {
            xi: 2.0,
            fx,
            fy,
            cx,
            cy,
            ypr_deg,
            t_m,
            terms,
        }
    };
    let v3 = |k, p| CalibTerms::V3 { k, p };
    let v6 = |d| CalibTerms::V6 { d };
    let raw = RawStrings {
        v1: None,
        v2: None,
        v3: Some("a V3 string of my own".into()),
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
            v3: Some([
                block(
                    0,
                    4284.66,
                    4284.24,
                    v3(
                        [0.18632057, 2.04984351, -3.24446913],
                        [-0.00143572, -0.00339246],
                    ),
                ),
                block(
                    1,
                    4291.07,
                    4291.01,
                    v3(
                        [0.18940832, 2.07073065, -3.24103338],
                        [0.00013837, -0.00299871],
                    ),
                ),
            ]),
            v6: Some([
                block(
                    0,
                    4277.56,
                    4277.23,
                    v6([
                        0.28032991,
                        0.89045402,
                        2.04927581,
                        -8.05179784,
                        0.0,
                        0.00120203,
                        -0.00259481,
                        0.02324702,
                        0.00533924,
                        -0.01560201,
                        0.00019353,
                        -0.04580527,
                        0.00197181,
                    ]),
                ),
                block(
                    1,
                    4284.44,
                    4284.72,
                    v6([
                        0.27665338,
                        0.98522977,
                        1.83605261,
                        -7.99374482,
                        0.0,
                        0.00021052,
                        -0.00147621,
                        0.01940429,
                        0.00331834,
                        -0.01330661,
                        0.00074517,
                        -0.02859293,
                        0.00888742,
                    ]),
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
            generation,
            v6: V6Reading::RADIAL_ONLY,
            status: Status::Provisional,
            evidence: "placeholder until the fit".into(),
        },
        tune: Tune::default(),
        gains: Gains::Off {},
        profile: None,
    }
}

/// Each corner of the frame's bands for one lens: radius scale, centre
/// shift on each axis and ξ's delta at their least and most, θ_max at
/// `theta_max_deg`.
fn frame_corners(radius: [f64; 2], theta_max_deg: f64) -> Vec<LensTune> {
    let ends = |e: f64| [-e, e];
    let mut out = Vec::new();
    for radius_scale in radius {
        for x in ends(band::CENTRE_PX) {
            for y in ends(band::CENTRE_PX) {
                for xi_delta in ends(band::XI_DELTA) {
                    out.push(LensTune {
                        centre_px: [x, y],
                        radius_scale,
                        xi_delta,
                        theta_max_deg,
                        ..LensTune::default()
                    });
                }
            }
        }
    }
    out
}

/// The least room, in degrees, that min(θ_max, θ_frame) leaves past 90°
/// over each lens of both generations at every corner of the frame's
/// bands with the radius scale's ends `radius`, with that corner; each
/// corner's rig built at the feather's and q's most. θ_frame falls as
/// the focal grows, as ξ shrinks and as the centre nears an edge, so its
/// least over the bands is at a corner; the lens's turn and q don't move
/// it.
fn frame_room(radius: [f64; 2]) -> (f64, LensTune) {
    let mut least = (f64::INFINITY, LensTune::default());
    for generation in [Generation::V3, Generation::V6] {
        for theta_max_deg in band::THETA_MAX_DEG {
            for corner in frame_corners(radius, theta_max_deg) {
                let mut d = base_source(generation);
                d.tune = Tune {
                    lenses: [corner; 2],
                    feather_deg: band::FEATHER_DEG,
                    q_per_m: band::Q_PER_M,
                };
                let rig = d
                    .rig([768; 2])
                    .unwrap_or_else(|e| panic!("{generation:?} at {corner:?}: {e}"));
                for lens in &rig.lenses {
                    let room = lens.theta_max.min(lens.theta_frame).to_degrees() - 90.0;
                    if room < least.0 {
                        least = (room, corner);
                    }
                }
            }
        }
    }
    least
}

/// How far, in degrees, the two lenses' reach exceeds what covering
/// every direction needs, by the sufficient rule `validate` applies
/// (11.2): each lens's min(θ_max, θ_frame) less asin(q·|c_b − o|), the
/// most parallax can turn its ray, about its own axis, the two summing
/// to at least 360° less the angle between the axes. On the great circle
/// through both axes a direction's angles to them sum to at most that,
/// and anywhere else to less.
fn coverage_slack(rig: &Rig) -> f64 {
    let reach = |l: &RigLens| l.theta_max.min(l.theta_frame) - (rig.q * l.centre_m.length()).asin();
    let axis = |l: &RigLens| l.lens_from_rig.inverse() * DVec3::Z;
    let [a, b] = &rig.lenses;
    let apart = axis(a).angle_between(axis(b));
    (reach(a) + reach(b) - (TAU - apart)).to_degrees()
}

/// The least [`coverage_slack`] over both generations, both bindings and
/// every corner of the two lenses' yaw, pitch and roll bands of ±`ypr`
/// degrees, at q `q`, θ_max `theta_max_deg` and each lens's frame bands
/// at `frame`, the corner where its θ_frame is least. The axes and the
/// lenses' centres don't move with the frame's bands, nor θ_frame with
/// the turn, which the loop checks at each corner, so the least over the
/// product of the two sets of corners is here.
fn least_coverage(ypr: f64, q: f64, theta_max_deg: f64, frame: LensTune) -> f64 {
    let ends = [-ypr, ypr];
    let mut least = f64::INFINITY;
    for generation in [Generation::V3, Generation::V6] {
        for binding in [Binding::Direct, Binding::Reversed] {
            let mut d = base_source(generation);
            d.geometry.binding = binding;
            let lens = LensTune {
                theta_max_deg,
                ..frame
            };
            d.tune = Tune {
                lenses: [lens; 2],
                feather_deg: band::FEATHER_DEG,
                q_per_m: q,
            };
            let untuned = d.rig([768; 2]).unwrap();
            for i in 0..64 {
                let turn = |bits: usize| [0, 1, 2].map(|k| ends[(bits >> k) & 1]);
                for (b, bits) in [i & 7, i >> 3].into_iter().enumerate() {
                    d.tune.lenses[b].ypr_deg = turn(bits);
                }
                let rig = d.rig([768; 2]).unwrap();
                for (l, u) in rig.lenses.iter().zip(&untuned.lenses) {
                    assert_eq!(l.theta_frame, u.theta_frame);
                    assert!((l.centre_m.length() - u.centre_m.length()).abs() < 1e-12);
                }
                least = least.min(coverage_slack(&rig));
            }
        }
    }
    least
}

/// The corners of the stitch property's tune draws clear `validate`'s
/// frame and coverage rules (11.2), which run at every apply: `Rig::build`
/// takes each (its refusals read only the frame's bands and θ_max), each
/// lens's min(θ_max, θ_frame) − 90° is at least the largest feather
/// drawn, and the two lenses cover every direction at the largest q
/// drawn, by [`coverage_slack`]'s rule. Controls: a radius scale of 1.1
/// fails the frame half; θ_max 95° on both lenses with one yawed 15° at
/// q 3.34 fails the coverage half.
#[test]
fn the_drawn_tunes_clear_the_frame() {
    let began = std::time::Instant::now();
    let (room, worst) = frame_room(band::RADIUS_SCALE);
    eprintln!("the frame's least room: {room:.3}° at {worst:?}");
    assert_eq!(
        at_least(room, band::FEATHER_DEG).passes(),
        Some(true),
        "the frame leaves {room:.3}° at {worst:?}"
    );
    let slack = least_coverage(band::YPR_DEG, band::Q_PER_M, band::THETA_MAX_DEG[0], worst);
    eprintln!("the coverage's least slack: {slack:.3}°");
    assert_eq!(at_least(slack, 0.0).passes(), Some(true), "{slack:.3}°");

    // Controls.
    let (room, at) = frame_room([band::RADIUS_SCALE[0], 1.1]);
    eprintln!("at a radius scale of 1.1: {room:.3}° at {at:?}");
    must_fail(
        "a radius scale of 1.1 clears the frame",
        at_least(room, band::FEATHER_DEG),
    );
    let mut d = base_source(Generation::V6);
    d.tune.q_per_m = 3.34;
    for lens in &mut d.tune.lenses {
        lens.theta_max_deg = 95.0;
    }
    d.tune.lenses[1].ypr_deg = [15.0, 0.0, 0.0];
    let slack = coverage_slack(&d.rig([768; 2]).unwrap());
    eprintln!("θ_max 95° with lens B yawed 15° at q 3.34: {slack:.3}°");
    must_fail(
        "θ_max 95° with lens B yawed 15° at q 3.34 covers every direction",
        at_least(slack, 0.0),
    );
    eprintln!(
        "the_drawn_tunes_clear_the_frame: {:.3} s",
        began.elapsed().as_secs_f64()
    );
}

/// One lens's tune, drawn inside [`band`].
fn lens_tune() -> impl Strategy<Value = LensTune> {
    (
        prop::array::uniform3(-band::YPR_DEG..=band::YPR_DEG),
        prop::array::uniform2(-band::CENTRE_PX..=band::CENTRE_PX),
        band::RADIUS_SCALE[0]..=band::RADIUS_SCALE[1],
        -band::XI_DELTA..=band::XI_DELTA,
        band::THETA_MAX_DEG[0]..=band::THETA_MAX_DEG[1],
    )
        .prop_map(
            |(ypr_deg, centre_px, radius_scale, xi_delta, theta_max_deg)| LensTune {
                ypr_deg,
                centre_px,
                radius_scale,
                xi_delta,
                theta_max_deg,
            },
        )
}

/// A whole tune, drawn inside [`band`].
fn tune() -> impl Strategy<Value = Tune> {
    (
        prop::array::uniform2(lens_tune()),
        0.0..=band::FEATHER_DEG,
        0.0..=band::Q_PER_M,
    )
        .prop_map(|(lenses, feather_deg, q_per_m)| Tune {
            lenses,
            feather_deg,
            q_per_m,
        })
}

/// A fingerprint of 16 hex digits from `n`.
fn fingerprint(n: u64) -> Fingerprint {
    Fingerprint {
        bytes: n >> 24,
        head: format!("{n:016x}"),
    }
}

/// Off, Fixed or Auto gains inside 11.2's bounds: each Fixed gain 0.5 to
/// 2 (11.2: 0.25 to 4), an Auto track at a positive rate with finite
/// ratios, measured under a tune of its own.
fn gains() -> impl Strategy<Value = Gains> {
    let fixed = prop::array::uniform2(prop::array::uniform3(0.5f64..2.0))
        .prop_map(|rgb| Gains::Fixed { rgb });
    let auto = (
        0.5f64..60.0,
        prop::collection::vec(prop::array::uniform3(-0.5f32..0.5), 1..24),
        tune(),
        any::<bool>(),
        any::<u64>(),
        0u32..4,
        any::<u32>(),
    )
        .prop_map(
            |(rate_hz, log_ratio, under, lrv, n, every, estimator)| Gains::Auto {
                rate_hz,
                log_ratio,
                basis: gains_basis(&under),
                input: AnalysisInput {
                    file: if lrv { Analysed::Lrv } else { Analysed::Master },
                    fingerprint: fingerprint(n),
                    frames: match every {
                        0 => Sampling::Keyframes {},
                        n => Sampling::Every { n },
                    },
                },
                estimator,
            },
        );
    prop_oneof![Just(Gains::Off {}), fixed, auto]
}

/// No profile, or one of four by name with a file of its own.
fn profile() -> impl Strategy<Value = Option<ProfileRef>> {
    prop::option::of(
        (0u8..4, any::<u64>())
            .prop_map(|(n, bytes)| ProfileRef::of(&format!("profile {n}"), &bytes.to_le_bytes())),
    )
}

/// An equirect source, or [`base_source`] at either generation with
/// either binding, any status and a drawn tune, gains and profile.
fn source() -> impl Strategy<Value = Source> {
    let camera = (
        any::<bool>(),
        0usize..4,
        any::<bool>(),
        tune(),
        gains(),
        profile(),
    )
        .prop_map(|(direct, status, v3, tune, gains, profile)| {
            let mut d = base_source(if v3 { Generation::V3 } else { Generation::V6 });
            d.geometry.binding = if direct {
                Binding::Direct
            } else {
                Binding::Reversed
            };
            d.geometry.status = [
                Status::Provisional,
                Status::Settled,
                Status::Fitted,
                Status::User,
            ][status];
            d.geometry.evidence = format!("drawn {:?}", d.geometry.status);
            d.tune = tune;
            d.gains = gains;
            d.profile = profile;
            Source::DualFisheye(Box::new(d))
        });
    prop_oneof![1 => Just(Source::Equirect {}), 3 => camera]
}

/// A cap inside 11.2's bounds (5° to 35°, an edge of 0° to 5°): a fill,
/// or a patch whose file sits beside the clips.
fn cap() -> impl Strategy<Value = Box<Cap>> {
    (
        any::<bool>(),
        0.0f64..10.0,
        any::<u64>(),
        5.0f64..=35.0,
        0.0f64..=5.0,
        -180.0f64..180.0,
    )
        .prop_map(|(fill, ring_deg, n, cap_deg, edge_deg, rotation_deg)| {
            Box::new(Cap {
                mode: if fill {
                    CapMode::Fill { ring_deg }
                } else {
                    CapMode::Patch {
                        path: format!("/clips/caps/patch_{}.png", n % 4).into(),
                        fingerprint: fingerprint(n),
                    }
                },
                cap_deg,
                edge_deg,
                rotation_deg,
            })
        })
}

fn caps() -> impl Strategy<Value = Caps> {
    (prop::option::of(cap()), prop::option::of(cap()))
        .prop_map(|(nadir, zenith)| Caps { nadir, zenith })
}

/// One stitch op's worth of randomness, its media still to pick.
#[derive(Debug, Clone)]
enum StitchDraw {
    Source(Source),
    Stitch(Option<Tune>, Option<Gains>, Option<ProfileRef>),
    Caps(Caps),
}

/// Which media (0 to 4 the camera file's, 5 to 8 the equirect file's, 9
/// one that isn't there) and the op.
fn stitch_draw() -> impl Strategy<Value = (u8, StitchDraw)> {
    let op = prop_oneof![
        source().prop_map(StitchDraw::Source),
        (
            prop::option::weighted(0.7, tune()),
            prop::option::weighted(0.7, gains()),
            profile()
        )
            .prop_map(|(tune, gains, profile)| StitchDraw::Stitch(tune, gains, profile)),
        caps().prop_map(StitchDraw::Caps),
    ];
    (0u8..10, op)
}

fn stitch_edit(media: [MediaId; 2], pick: u8, draw: &StitchDraw) -> Edit {
    let media = match pick {
        0..=4 => media[0],
        5..=8 => media[1],
        _ => MediaId(999),
    };
    match draw.clone() {
        StitchDraw::Source(source) => Edit::SetSource { media, source },
        StitchDraw::Stitch(tune, gains, profile) => Edit::SetStitch {
            media,
            tune,
            gains,
            profile,
        },
        StitchDraw::Caps(caps) => Edit::SetCaps { media, caps },
    }
}

/// The refusal the stitch ops' own rules give `edit` on `p`, if any: a
/// `set_stitch` with neither a tune nor gains, or with a profile but no
/// tune, is Invalid whatever its media; a media that isn't there is
/// NotFound; a `set_stitch` on a media that isn't two fisheye lenses is
/// Invalid.
fn ruled_out(p: &Project, edit: &Edit) -> Option<Code> {
    let (media, stitch) = match edit {
        Edit::SetSource { media, .. } | Edit::SetCaps { media, .. } => (*media, false),
        Edit::SetStitch {
            media,
            tune,
            gains,
            profile,
        } => {
            if tune.is_none() && (gains.is_none() || profile.is_some()) {
                return Some(Code::Invalid);
            }
            (*media, true)
        }
        other => unreachable!("{other:?} is no stitch op"),
    };
    match p.media_by_id(media) {
        None => Some(Code::NotFound),
        Some(m) if stitch && !matches!(m.source, Source::DualFisheye(_)) => Some(Code::Invalid),
        Some(_) => None,
    }
}

/// The stitch property's project: a 10 s camera file of two 5376² lenses
/// whose source is [`base_source`] at V6, made as `project_media` makes
/// one, then a 10 s equirect file added. Returns it and the two media.
fn stitch_project() -> (Project, [MediaId; 2]) {
    let mut m = common::media("/clips/VID_20250101_120000_00_001.insv", 10);
    m.video.width = 5376;
    m.video.height = 5376;
    m.source = Source::DualFisheye(Box::new(base_source(Generation::V6)));
    let mut p = Project::untitled(
        m,
        InitialView {
            orientation: ypr(20.0, -5.0, 0.0),
            fov_deg: 90.0,
            projection: Projection::Rectilinear,
        },
    )
    .unwrap();
    let camera = p.the_clip().media;
    p.apply(&[Edit::AddMedia {
        media: common::media("/clips/pano.mp4", 10),
    }])
    .unwrap();
    let flat = p
        .media()
        .iter()
        .map(|m| m.id)
        .find(|&id| id != camera)
        .unwrap();
    (p, [camera, flat])
}

/// Whether `inverse`, applied to `after`, gives back `before`'s content.
fn undoes(after: &Project, inverse: &[Edit], before: &Project) -> bool {
    let mut p = after.clone();
    p.apply(inverse).is_ok() && p.same_content(before)
}

/// The gains of media `id` in `p`, if it has two fisheye lenses.
fn gains_of(p: &Project, id: MediaId) -> Option<&Gains> {
    match &p.media_by_id(id)?.source {
        Source::DualFisheye(d) => Some(&d.gains),
        _ => None,
    }
}

/// `p` saved as a project file's text in `/clips` and loaded again.
fn saved_and_loaded(p: &Project) -> Project {
    let folder = Path::new("/clips");
    file::from_json(&file::to_json(p, folder).unwrap(), folder).unwrap()
}

/// The control on the undo check: `set_stitch`'s inverse `inverse`,
/// applied to `after`, with its gains dropped, must not give back
/// `before` when the edit changed the gains.
fn dropping_the_gains_is_caught(before: &Project, after: &Project, inverse: &[Edit]) -> bool {
    let [
        Edit::SetStitch {
            media,
            tune,
            gains: Some(_),
            profile,
        },
    ] = inverse
    else {
        return false;
    };
    if gains_of(before, *media) == gains_of(after, *media) {
        return false;
    }
    let dropped = Edit::SetStitch {
        media: *media,
        tune: tune.clone(),
        gains: None,
        profile: profile.clone(),
    };
    must_fail(
        "set_stitch's inverse without its gains undoes it",
        undoes(after, &[dropped], before),
    );
    true
}

/// The stitch property's cases: `PROPTEST_CASES`, or 64 when it is
/// unset, a quarter of the other properties' 256, since a case saves and
/// loads a project of two calibrations at each step it takes (about 3 ms
/// a case in a debug build, 90% of it the file's text). Its gate is a run
/// at `PROPTEST_CASES=100000`.
const STITCH_CASES: u32 = 64;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(STITCH_CASES)
    ))]

    /// Random tunes, gains, sources and caps through `set_source`,
    /// `set_stitch` and `set_caps` in random order (M3 plan 11.5), on a
    /// camera file and an equirect one: a step the ops' own rules refuse
    /// changes nothing, and no other is refused; each step's inverse
    /// undoes it exactly; the project saves and loads as itself at every
    /// step it takes (one case saving the first); undoing all gives the
    /// first project and redoing all the last. No case builds a rig. Control: an inverse of `set_stitch`
    /// that drops its gains fails the undo check, on the first project
    /// with Fixed gains of 0.9 on lens B and at each drawn edit that
    /// changes the gains.
    #[test]
    fn stitch_edits_undo_and_save(steps in prop::collection::vec(prop::collection::vec(stitch_draw(), 1..=2), 1..=8)) {
        let (mut p, media) = stitch_project();
        let original = p.clone();
        // The first project is every case's, so one case saves it.
        static FIRST: std::sync::Once = std::sync::Once::new();
        FIRST.call_once(|| assert_eq!(saved_and_loaded(&original), original));

        let mut fixed = p.clone();
        let applied = fixed
            .apply(&[Edit::SetStitch {
                media: media[0],
                tune: None,
                gains: Some(Gains::Fixed { rgb: [[1.0; 3], [0.9; 3]] }),
                profile: None,
            }])
            .unwrap();
        prop_assert!(undoes(&fixed, &applied.inverse, &p));
        prop_assert!(dropping_the_gains_is_caught(&p, &fixed, &applied.inverse));

        let mut h = History::new();
        let mut states = vec![p.clone()];
        for draws in &steps {
            let edits: Vec<Edit> = draws.iter().map(|(pick, d)| stitch_edit(media, *pick, d)).collect();
            // Each edit is judged on what the edits before it leave.
            let mut scratch = p.clone();
            let mut ruled = None;
            for (i, e) in edits.iter().enumerate() {
                if let Some(code) = ruled_out(&scratch, e) {
                    ruled = Some((i, code));
                    break;
                }
                let was = scratch.clone();
                match scratch.apply(std::slice::from_ref(e)) {
                    Ok(applied) => {
                        prop_assert!(undoes(&scratch, &applied.inverse, &was), "{:?}", e);
                        dropping_the_gains_is_caught(&was, &scratch, &applied.inverse);
                    }
                    Err(err) => prop_assert!(false, "{:?} refused by no rule of the stitch ops: {}", e, err),
                }
            }
            let before = p.clone();
            match (h.record(&mut p, "Stitch", Origin::User, &edits), ruled) {
                (Ok(step), None) => {
                    prop_assert!(undoes(&p, &step.inverse, &before));
                    prop_assert!(p.same_content(&scratch));
                    prop_assert_eq!(&saved_and_loaded(&p), &p);
                    states.push(p.clone());
                }
                // The project is the one before, which saved as itself.
                (Err(e), Some((i, code))) => {
                    prop_assert_eq!((e.index, e.code()), (i, code), "{}", e);
                    prop_assert_eq!(&p, &before, "a refused step changes nothing");
                }
                (Ok(_), Some((i, code))) => prop_assert!(false, "edit {} applied where {} was due", i, code),
                (Err(e), None) => prop_assert!(false, "refused by no rule of the stitch ops: {}", e),
            }
        }
        for state in states[..states.len() - 1].iter().rev() {
            prop_assert!(h.undo(&mut p).unwrap().is_some());
            prop_assert!(p.same_content(state));
        }
        prop_assert!(h.undo(&mut p).unwrap().is_none());
        prop_assert!(p.same_content(&original));
        for state in &states[1..] {
            prop_assert!(h.redo(&mut p).unwrap().is_some());
            prop_assert!(p.same_content(state));
        }
        prop_assert!(h.redo(&mut p).unwrap().is_none());
    }
}
