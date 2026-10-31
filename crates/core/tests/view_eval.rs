//! The view between keys, by example (M2 plan section 6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::ypr;
use glam::DVec3;
use spherecut_core::project::*;
use spherecut_core::{Rational, Time};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, LensShape, Morph, Projection};
use spherecut_sphere::{angle_between, rotation_angle};

fn at(ms: i64) -> Time {
    Time(Rational::new(ms, 1000).unwrap())
}

fn key<V>(id: u64, ms: i64, value: V, ease: Ease) -> Key<V> {
    Key {
        id: KeyId(id),
        at: at(ms),
        value,
        ease,
        spin: 0,
    }
}

fn view(o: &[(i64, Orient)], ease: Ease, mode: OrientMode) -> ViewAnim {
    ViewAnim {
        mode,
        orientation: KeyTrack {
            keys: o
                .iter()
                .enumerate()
                .map(|(i, &(ms, v))| key(i as u64 + 1, ms, v, ease))
                .collect(),
        },
        fov: KeyTrack::one(KeyId(100), 90.0, Ease::SMOOTH),
        projection: KeyTrack::one(
            KeyId(101),
            LensShape::Pure(Projection::Rectilinear),
            Ease::STOP,
        ),
    }
}

fn look(yaw: f64, pitch: f64, roll: f64) -> Orient {
    Orient::View(ypr(yaw, pitch, roll))
}

fn angles(v: &ViewAnim, t: f64, targets: &[Target]) -> [f64; 3] {
    let y = Ypr::from_quat(v.orientation_at(t, targets));
    [y.yaw, y.pitch, y.roll].map(f64::to_degrees)
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn a_level_pan_keeps_the_horizon_and_free_takes_the_geodesic() {
    // The plan's measurement: (0°, 0°) to (90°, 30°).
    let keys = [(0, look(0.0, 0.0, 0.0)), (2000, look(90.0, 30.0, 0.0))];
    let level = view(&keys, Ease::LINEAR, OrientMode::Level);
    let [y, p, r] = angles(&level, 1.0, &[]);
    assert!(close(y, 45.0, 1e-9) && close(p, 15.0, 1e-9) && close(r, 0.0, 1e-9));
    let free = view(&keys, Ease::LINEAR, OrientMode::Free);
    let [_, _, r] = angles(&free, 1.0, &[]);
    assert!(close(r, -6.5, 0.05), "slerp midpoint roll {r}");
}

#[test]
fn tracks_hold_their_ends_and_hit_their_keys() {
    let keys = [
        (1000, look(10.0, 0.0, 0.0)),
        (2000, look(60.0, 20.0, 5.0)),
        (4000, look(-30.0, -10.0, 0.0)),
    ];
    for mode in [OrientMode::Level, OrientMode::Free] {
        let v = view(&keys, Ease::SMOOTH, mode);
        for (ms, o) in keys {
            let Orient::View(o) = o else { unreachable!() };
            let t = ms as f64 / 1000.0;
            assert!(rotation_angle(v.orientation_at(t, &[]), o.quat()) < 1e-12);
        }
        let first = v.orientation_at(0.0, &[]);
        assert!(rotation_angle(first, ypr(10.0, 0.0, 0.0).quat()) < 1e-12);
        let last = v.orientation_at(9.0, &[]);
        assert!(rotation_angle(last, ypr(-30.0, -10.0, 0.0).quat()) < 1e-12);
    }
}

#[test]
fn spin_adds_whole_turns_of_yaw() {
    let keys = [(0, look(0.0, 0.0, 0.0)), (1000, look(10.0, 0.0, 0.0))];
    let mut v = view(&keys, Ease::LINEAR, OrientMode::Level);
    assert!(close(angles(&v, 0.5, &[])[0], 5.0, 1e-9));
    v.orientation.keys[0].spin = 1;
    assert!(
        close(angles(&v, 0.5, &[])[0], -175.0, 1e-9),
        "185° is −175°"
    );
    v.orientation.keys[0].spin = -1;
    assert!(close(angles(&v, 0.5, &[])[0], -175.0, 1e-9), "−175°");
    assert!(
        close(angles(&v, 0.25, &[])[0], -87.5, 1e-9),
        "the long way left"
    );
}

#[test]
fn a_zoom_is_even_in_apparent_size() {
    let mut v = view(&[(0, look(0.0, 0.0, 0.0))], Ease::SMOOTH, OrientMode::Level);
    v.fov = KeyTrack {
        keys: vec![
            key(1, 0, 20.0, Ease::LINEAR),
            key(2, 2000, 80.0, Ease::LINEAR),
        ],
    };
    assert!(close(v.fov_at(1.0).to_degrees(), 40.0, 1e-9));
    assert!(close(v.fov_at(0.0).to_degrees(), 20.0, 1e-9));
    assert!(close(v.fov_at(5.0).to_degrees(), 80.0, 1e-9));
}

/// How much of `p` a lens blends in.
fn weight(l: Lens, p: Projection) -> f64 {
    let mut w = 0.0;
    if l.from == p {
        w += 1.0 - l.morph;
    }
    if l.to == p {
        w += l.morph;
    }
    w
}

#[test]
fn a_projection_change_morphs_by_smoothstep() {
    use Projection::*;
    let mut v = view(&[(0, look(0.0, 0.0, 0.0))], Ease::SMOOTH, OrientMode::Level);
    v.projection = KeyTrack {
        keys: vec![
            key(1, 0, LensShape::Pure(Rectilinear), Ease::STOP),
            key(2, 1000, LensShape::Pure(Stereographic), Ease::STOP),
            key(3, 2000, LensShape::Pure(Stereographic), Ease::STOP),
        ],
    };
    let l = v.lens_at(0.25);
    assert!(close(weight(l, Stereographic), 0.15625, 1e-12));
    assert!(close(weight(l, Rectilinear), 0.84375, 1e-12));
    assert!(close(weight(v.lens_at(0.5), Stereographic), 0.5, 1e-12));
    assert_eq!(v.lens_at(1.5).shape(), LensShape::Pure(Stereographic));
    // Linear sides morph at a constant rate.
    v.projection.keys[0].ease = Ease::LINEAR;
    v.projection.keys[1].ease = Ease::LINEAR;
    assert!(close(weight(v.lens_at(0.25), Stereographic), 0.25, 1e-12));
    // Hold keeps the first projection until the next key.
    v.projection.keys[0].ease = Ease::HOLD;
    assert_eq!(v.lens_at(0.999).shape(), LensShape::Pure(Rectilinear));
    assert_eq!(v.lens_at(1.0).shape(), LensShape::Pure(Stereographic));
    assert!(close(v.lens_at(1.0).hfov.to_degrees(), 90.0, 1e-12));
}

#[test]
fn a_half_morphed_view_keys_and_plays_back_as_it_was() {
    use Projection::*;
    // The viewer, part way through a morph, as D33's slider leaves it.
    let seen = Lens {
        from: Fisheye,
        to: Rectilinear,
        morph: 0.3,
        hfov: 1.5,
    };
    let mut v = view(&[(0, look(0.0, 0.0, 0.0))], Ease::SMOOTH, OrientMode::Level);
    v.fov = KeyTrack::one(KeyId(100), 1.5f64.to_degrees(), Ease::SMOOTH);
    v.projection = KeyTrack {
        keys: vec![
            key(1, 0, LensShape::Pure(Stereographic), Ease::STOP),
            key(2, 1000, seen.shape(), Ease::STOP),
        ],
    };
    let back = v.lens_at(1.0);
    for rho in [0.1, 0.5, 0.9, 1.0] {
        assert!(close(back.theta(rho), seen.theta(rho), 1e-12));
    }
    // On the way there, two projections at a time: stereographic is next
    // to both, so the morph runs from it towards the keyed blend.
    let mid = v.lens_at(0.5);
    assert!(
        mid.from == Stereographic || mid.to == Stereographic,
        "{mid:?}"
    );
    // Keys of morphs in a row morph on through: rectilinear, a blend of
    // fisheye and stereographic, then rectilinear again.
    v.projection = KeyTrack {
        keys: vec![
            key(1, 0, LensShape::Pure(Rectilinear), Ease::SMOOTH),
            key(
                2,
                1000,
                LensShape::Morph(Morph {
                    from: Fisheye,
                    to: Stereographic,
                    morph: 0.5,
                }),
                Ease::SMOOTH,
            ),
            key(3, 2000, LensShape::Pure(Rectilinear), Ease::SMOOTH),
        ],
    };
    // The blend is 1.5 from rectilinear either way round: the tie goes
    // forward, through fisheye, and rectilinear after it is 1.5 on
    // again, so the morph carries on round through stereographic.
    let early = v.lens_at(0.5);
    assert_eq!((early.from, early.to), (Rectilinear, Fisheye), "{early:?}");
    let late = v.lens_at(1.5);
    assert!(
        weight(late, Stereographic) > 0.0 && weight(late, Fisheye) < 0.5,
        "{late:?}"
    );
}

fn moving_target() -> Target {
    let dir = |yaw_deg, pitch_deg| AimDir { yaw_deg, pitch_deg };
    Target {
        id: TargetId(50),
        name: "rider".into(),
        media: MediaId(1),
        path: KeyTrack {
            keys: vec![
                key(51, 0, dir(170.0, 0.0), Ease::SMOOTH),
                key(52, 2000, dir(-150.0, 20.0), Ease::SMOOTH),
                key(53, 4000, dir(-90.0, 89.5), Ease::SMOOTH),
            ],
        },
    }
}

fn forward(q: glam::DQuat) -> DVec3 {
    q * DVec3::NEG_Z
}

fn aim_dir(a: Aim) -> DVec3 {
    forward(Ypr::new(a.yaw, a.pitch, 0.0).to_quat())
}

#[test]
fn a_target_takes_the_short_way_and_clamps_its_pitch() {
    let t = moving_target();
    // 170° to −150° is 40° the short way, through 180°.
    let mid = t.at(1.0);
    assert!(
        mid.yaw.to_degrees().abs() > 170.0,
        "{}",
        mid.yaw.to_degrees()
    );
    assert!(mid.yaw_rate > 0.0);
    let end = t.at(4.0);
    assert!(close(end.pitch.to_degrees(), MAX_AIM_PITCH_DEG, 1e-12));
    assert_eq!(end.pitch_rate, 0.0);
}

#[test]
fn one_aim_key_follows_its_target_all_the_way() {
    let targets = [moving_target()];
    let aim = Orient::Aim {
        target: TargetId(50),
        roll_deg: 0.0,
    };
    for mode in [OrientMode::Level, OrientMode::Free] {
        let v = view(&[(1000, aim)], Ease::SMOOTH, mode);
        for i in 0..=50 {
            let s = f64::from(i) / 10.0;
            let got = forward(v.orientation_at(s, &targets));
            assert!(angle_between(got, aim_dir(targets[0].at(s))) < 1e-9, "{s}");
        }
    }
}

#[test]
fn two_aim_keys_follow_the_target_with_the_roll_keyed() {
    let targets = [moving_target()];
    let aim = |roll_deg| Orient::Aim {
        target: TargetId(50),
        roll_deg,
    };
    let v = view(
        &[
            (0, look(0.0, 0.0, 0.0)),
            (1000, aim(0.0)),
            (3000, aim(20.0)),
        ],
        Ease::LINEAR,
        OrientMode::Level,
    );
    let q = v.orientation_at(2.0, &targets);
    assert!(angle_between(forward(q), aim_dir(targets[0].at(2.0))) < 1e-9);
    assert!(close(Ypr::from_quat(q).roll.to_degrees(), 10.0, 1e-9));
}

#[test]
fn sticky_spin_keeps_a_turn_going_past_half_way() {
    let make = |yaw| {
        view(
            &[(0, look(0.0, 0.0, 0.0)), (1000, look(yaw, 0.0, 0.0))],
            Ease::LINEAR,
            OrientMode::Level,
        )
    };
    let before = make(179.995);
    let mut after = make(180.005);
    // Without the spin the turn goes the other way.
    assert!(angles(&after, 0.5, &[])[0] < 0.0);
    let spins = follow_spin(&before, &after, &[]);
    assert_eq!(spins, vec![(KeyId(1), 1)]);
    after.orientation.keys[0].spin = 1;
    assert!(close(angles(&after, 0.5, &[])[0], 90.0025, 1e-9));
    // Nothing changed, nothing to do.
    assert!(follow_spin(&after, &after, &[]).is_empty());
}

/// Section 6's speed bound: a view on long tracks in under 5 µs, the
/// slowest kind of segment (Free, aimed at a moving target, with a
/// morph) included.
#[test]
fn a_view_is_quick_to_evaluate() {
    let dir = |yaw_deg, pitch_deg| AimDir { yaw_deg, pitch_deg };
    let target = Target {
        id: TargetId(50),
        name: "rider".into(),
        media: MediaId(1),
        path: KeyTrack {
            keys: (0..1000)
                .map(|i| {
                    key(
                        10_000 + i,
                        i as i64 * 100,
                        dir((i * 37 % 360) as f64 - 180.0, 10.0),
                        Ease::SMOOTH,
                    )
                })
                .collect(),
        },
    };
    let targets = [target];
    let orient = |i: u64| {
        if i.is_multiple_of(3) {
            Orient::Aim {
                target: TargetId(50),
                roll_deg: 5.0,
            }
        } else {
            look((i * 53 % 360) as f64 - 180.0, (i % 50) as f64 - 25.0, 0.0)
        }
    };
    for mode in [OrientMode::Level, OrientMode::Free] {
        let v = ViewAnim {
            mode,
            orientation: KeyTrack {
                keys: (0..10_000)
                    .map(|i| key(i + 1, i as i64 * 10, orient(i), Ease::SMOOTH))
                    .collect(),
            },
            fov: KeyTrack {
                keys: (0..1000)
                    .map(|i| {
                        key(
                            20_000 + i,
                            i as i64 * 100,
                            40.0 + (i % 7) as f64 * 10.0,
                            Ease::SMOOTH,
                        )
                    })
                    .collect(),
            },
            projection: KeyTrack {
                keys: (0..100)
                    .map(|i| {
                        let p = [
                            Projection::Rectilinear,
                            Projection::Fisheye,
                            Projection::Stereographic,
                        ][i as usize % 3];
                        key(30_000 + i, i as i64 * 1000, LensShape::Pure(p), Ease::STOP)
                    })
                    .collect(),
            },
        };
        let n = 20_000;
        // The best of three, so a busy machine doesn't fail it.
        let each = (0..3)
            .map(|_| {
                let start = std::time::Instant::now();
                let mut sink = 0.0;
                for i in 0..n {
                    let t = f64::from(i) * 99.99 / f64::from(n);
                    sink += v.at(t, &targets).orientation.w;
                }
                assert!(sink.is_finite());
                start.elapsed() / n
            })
            .min()
            .unwrap();
        eprintln!("{mode:?}: {each:?} a view");
        assert!(
            each < std::time::Duration::from_micros(5),
            "{mode:?}: {each:?} a view"
        );
    }
}
