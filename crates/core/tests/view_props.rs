//! Property tests of the view between keys (M2 plan section 6.6), in
//! both orientation modes, on random tracks: 2 to 12 keys spaced with
//! ratios up to 20:1, values including pairs 179° to 181° apart, random
//! eases and spins. `PROPTEST_CASES` sets the count (100,000 before a
//! merge).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::f64::consts::TAU;
use std::path::Path;

use common::ypr;
use glam::{DQuat, DVec3};
use proptest::prelude::*;
use spherecut_core::project::file;
use spherecut_core::project::*;
use spherecut_core::{Rational, Time};
use spherecut_sphere::angle_between;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{LensShape, Projection};
use spherecut_sphere::rotation_angle;
use spherecut_sphere::spline::{chord, exp, log, wrap};

const TARGET: TargetId = TargetId(900);

/// One key's worth of randomness.
#[derive(Debug, Clone, Copy)]
struct Seed {
    /// Milliseconds after the previous key.
    gap_ms: i64,
    yaw: f64,
    pitch: f64,
    roll: f64,
    /// Put this key 179° to 181° on from the last (in yaw for Level,
    /// about a random axis for Free).
    far: Option<f64>,
    ease: Ease,
    spin: i8,
    aim: bool,
}

fn ease() -> impl Strategy<Value = Ease> {
    prop_oneof![
        4 => Just(Ease::SMOOTH),
        2 => Just(Ease::STOP),
        2 => Just(Ease::LINEAR),
        1 => Just(Ease::ARRIVE),
        1 => Just(Ease::LEAVE),
        1 => Just(Ease::HOLD),
    ]
}

fn seed(level_roll: bool) -> impl Strategy<Value = Seed> {
    (
        100i64..=2000,
        -180.0f64..180.0,
        -80.0f64..80.0,
        -180.0f64..180.0,
        prop::option::weighted(0.2, 179.0f64..181.0),
        ease(),
        prop_oneof![6 => Just(0i8), 1 => -2i8..=2],
        prop::bool::weighted(0.4),
    )
        .prop_map(
            move |(gap_ms, yaw, pitch, roll, far, ease, spin, aim)| Seed {
                gap_ms,
                yaw,
                pitch,
                roll: if level_roll { 0.0 } else { roll },
                far,
                ease,
                spin,
                aim,
            },
        )
}

fn mode() -> impl Strategy<Value = OrientMode> {
    prop_oneof![Just(OrientMode::Level), Just(OrientMode::Free)]
}

fn ms(t: i64) -> Time {
    Time(Rational::new(t, 1000).unwrap())
}

/// A view from seeds: keys at the seeds' gaps, aim keys only if `aims`.
fn build(seeds: &[Seed], mode: OrientMode, axis: DVec3, aims: bool) -> ViewAnim {
    let mut keys: Vec<Key<Orient>> = Vec::new();
    let mut t = 0;
    for (i, s) in seeds.iter().enumerate() {
        let mut o = ypr(s.yaw, s.pitch, s.roll);
        if let (
            Some(far),
            Some(Key {
                value: Orient::View(prev),
                ..
            }),
        ) = (s.far, keys.last())
        {
            o = match mode {
                OrientMode::Level => {
                    let p = prev.ypr();
                    Orientation::from_ypr(Ypr::new(p.yaw + far.to_radians(), p.pitch, p.roll))
                        .unwrap()
                }
                OrientMode::Free => {
                    Orientation::new(exp(axis * far.to_radians()) * prev.quat()).unwrap()
                }
            };
        }
        let value = if aims && s.aim {
            Orient::Aim {
                target: TARGET,
                roll_deg: s.roll,
            }
        } else {
            Orient::View(o)
        };
        keys.push(Key {
            id: KeyId(i as u64 + 1),
            at: ms(t),
            value,
            ease: s.ease,
            spin: s.spin,
        });
        t += s.gap_ms;
    }
    ViewAnim {
        mode,
        orientation: KeyTrack { keys },
        fov: KeyTrack::one(KeyId(100), 90.0, Ease::SMOOTH),
        projection: KeyTrack::one(
            KeyId(101),
            LensShape::Pure(Projection::Rectilinear),
            Ease::STOP,
        ),
    }
}

/// A target moving through 1 to 5 smooth keys over 0 to `end_ms`.
fn target(path: &[(f64, f64, f64)], end_ms: i64) -> Target {
    let mut keys: Vec<Key<AimDir>> = Vec::new();
    for (i, &(f, yaw_deg, pitch_deg)) in path.iter().enumerate() {
        // Spread the keys out, in increasing time.
        let at = (end_ms as f64 * (i as f64 + f) / path.len() as f64) as i64;
        keys.push(Key {
            id: KeyId(901 + i as u64),
            at: ms(at),
            value: AimDir { yaw_deg, pitch_deg },
            ease: Ease::SMOOTH,
            spin: 0,
        });
    }
    Target {
        id: TARGET,
        name: "target".into(),
        media: MediaId(1),
        path: KeyTrack { keys },
    }
}

/// Target paths, some going past the ±89° a view aims to.
fn path() -> impl Strategy<Value = Vec<(f64, f64, f64)>> {
    let pitch = prop_oneof![4 => -60.0f64..60.0, 1 => 85.0f64..=90.0, 1 => -90.0f64..=-85.0];
    prop::collection::vec((0.0f64..0.9, -180.0f64..180.0, pitch), 1..=5)
}

fn secs<V>(k: &Key<V>) -> f64 {
    k.at.to_secs_f64()
}

fn keys(v: &ViewAnim) -> &[Key<Orient>] {
    &v.orientation.keys
}

/// The angular velocity just after (`dir` 1) or before (−1) `t`, by
/// second-order one-sided differences at 1 µs.
fn omega(v: &ViewAnim, targets: &[Target], t: f64, dir: f64) -> DVec3 {
    let h = 1e-6 * dir;
    let q0 = v.orientation_at(t, targets);
    let r = |s: f64| log(v.orientation_at(t + s, targets) * q0.conjugate());
    (r(h) * 4.0 - r(2.0 * h)) / (2.0 * h)
}

/// The speed bound of section 6.6's no-flip rule: the largest chord
/// speed (Level: of yaw, pitch and roll together; Free: of the rotation).
fn chord_speed(v: &ViewAnim, targets: &[Target], i: usize) -> f64 {
    let (a, b) = (&keys(v)[i], &keys(v)[i + 1]);
    let (qa, qb) = (
        v.orientation_at(secs(a), targets),
        v.orientation_at(secs(b), targets),
    );
    let h = secs(b) - secs(a);
    match v.mode {
        OrientMode::Level => {
            let (ya, yb) = (Ypr::from_quat(qa), Ypr::from_quat(qb));
            let dyaw = wrap(yb.yaw - ya.yaw) + TAU * f64::from(a.spin);
            (dyaw.abs() + (yb.pitch - ya.pitch).abs() + wrap(yb.roll - ya.roll).abs()) / h
        }
        OrientMode::Free => chord(qa, qb, a.spin).length() / h,
    }
}

fn smooth_or_stop(e: Ease) -> bool {
    e.into == e.out && matches!(e.into, Side::Smooth | Side::Stop)
}

/// Whether the view should be C¹ at key `i`: the key's sides match, and
/// no hold makes a jump or a stop there. At the ends the track holds,
/// so the inner side must start or end at rest.
fn c1_expected(ks: &[Key<Orient>], i: usize) -> bool {
    let k = &ks[i];
    let aim = matches!(k.value, Orient::Aim { .. });
    if i == 0 || i == ks.len() - 1 {
        // Past the end a view key holds still and an aim key follows its
        // target, so the inner side must be at rest or, for an aim key,
        // at the target's rate.
        let side = if i == 0 { k.ease.out } else { k.ease.into };
        let at_rest = side == Side::Smooth || (side == Side::Stop && !aim);
        return at_rest && (i == 0 || !ks[i - 1].ease.hold);
    }
    let ok = if aim {
        // Stop at an aim key stops only the segment the target doesn't
        // drive.
        k.ease.into == Side::Smooth && k.ease.out == Side::Smooth
    } else {
        smooth_or_stop(k.ease)
    };
    ok && !k.ease.hold && !ks[i - 1].ease.hold
}

fn check_c1(v: &ViewAnim, targets: &[Target]) -> Result<(), TestCaseError> {
    let ks = keys(v);
    for i in 0..ks.len() {
        if !c1_expected(ks, i) {
            continue;
        }
        let t = secs(&ks[i]);
        let (l, r) = (omega(v, targets, t, -1.0), omega(v, targets, t, 1.0));
        let mut scale = l.length().max(r.length());
        if i > 0 {
            scale = scale.max(chord_speed(v, targets, i - 1));
        }
        if i + 1 < ks.len() {
            scale = scale.max(chord_speed(v, targets, i));
        }
        prop_assert!(
            (l - r).length() <= 1e-5 * scale + 1e-8,
            "key {i} at {t}: {l} before, {r} after ({:?})",
            ks[i].ease
        );
    }
    Ok(())
}

/// Samples of segment `i`, `n` steps, the end included unless the
/// segment holds (then it jumps there).
fn samples(v: &ViewAnim, targets: &[Target], i: usize, n: u32) -> Vec<(f64, DQuat)> {
    let ks = keys(v);
    let (t0, t1) = (secs(&ks[i]), secs(&ks[i + 1]));
    let last = if ks[i].ease.hold { n - 1 } else { n };
    (0..=last)
        .map(|j| {
            let t = t0 + (t1 - t0) * f64::from(j) / f64::from(n);
            (t, v.orientation_at(t, targets))
        })
        .collect()
}

proptest! {
    #[test]
    fn keys_are_hit_without_flips_and_c1_where_sides_match(
        seeds in prop::collection::vec(seed(false), 2..=12),
        mode in mode(),
        axis in (-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0),
    ) {
        let axis = DVec3::from(axis).try_normalize().unwrap_or(DVec3::Y);
        let v = build(&seeds, mode, axis, false);
        let ks = keys(&v);
        for k in ks {
            let Orient::View(o) = k.value else { unreachable!() };
            prop_assert!(rotation_angle(v.orientation_at(secs(k), &[]), o.quat()) < 1e-9);
        }
        check_c1(&v, &[])?;
        let fastest = (0..ks.len() - 1).map(|i| chord_speed(&v, &[], i)).fold(0.0, f64::max);
        for i in 0..ks.len() - 1 {
            let s = samples(&v, &[], i, 1000);
            let step = s[1].0 - s[0].0;
            for w in s.windows(2) {
                let turn = rotation_angle(w[0].1, w[1].1);
                prop_assert!(
                    turn <= 3.0 * fastest * step * (1.0 + 1e-9) + 1e-12,
                    "segment {i} at {}: {turn} in one step, fastest chord {fastest}", w[1].0
                );
            }
        }
    }

    #[test]
    fn a_segment_turns_its_spin(
        seeds in prop::collection::vec(seed(false), 2..=6),
        mode in mode(),
    ) {
        let v = build(&seeds, mode, DVec3::Y, false);
        let ks = keys(&v);
        for i in 0..ks.len() - 1 {
            let (a, b) = (&ks[i], &ks[i + 1]);
            if a.ease.hold {
                continue;
            }
            let s = samples(&v, &[], i, 2000);
            let (qa, qb) = (s[0].1, s[s.len() - 1].1);
            match mode {
                OrientMode::Level => {
                    let yaw = |q| Ypr::from_quat(q).yaw;
                    let total: f64 = s.windows(2).map(|w| wrap(yaw(w[1].1) - yaw(w[0].1))).sum();
                    let want = wrap(yaw(qb) - yaw(qa)) + TAU * f64::from(a.spin);
                    prop_assert!((total - want).abs() < 1e-6, "segment {i}: {total} for {want}");
                }
                OrientMode::Free => {
                    // Velocities along the chord make the geodesic.
                    let along = |side: Side, end: bool| {
                        side != Side::Smooth || if end { i + 2 == ks.len() } else { i == 0 }
                    };
                    if !(along(a.ease.out, false) && along(b.ease.into, true)) {
                        continue;
                    }
                    let total: f64 = s.windows(2).map(|w| rotation_angle(w[0].1, w[1].1)).sum();
                    let want = chord(qa, qb, a.spin).length();
                    prop_assert!((total - want).abs() < 1e-6, "segment {i}: {total} for {want}");
                }
            }
        }
    }

    #[test]
    fn linear_free_segments_are_slerp(
        seeds in prop::collection::vec(seed(false), 2..=6),
        stop in any::<bool>(),
    ) {
        let mut seeds = seeds;
        for s in &mut seeds {
            s.ease = if stop { Ease::STOP } else { Ease::LINEAR };
            s.spin = 0;
        }
        let v = build(&seeds, OrientMode::Free, DVec3::Y, false);
        let ks = keys(&v);
        for i in 0..ks.len() - 1 {
            for (t, q) in samples(&v, &[], i, 100) {
                let (qa, qb) = (v.orientation_at(secs(&ks[i]), &[]), v.orientation_at(secs(&ks[i + 1]), &[]));
                let u = (t - secs(&ks[i])) / (secs(&ks[i + 1]) - secs(&ks[i]));
                let u = if stop { u * u * (3.0 - 2.0 * u) } else { u };
                let want = exp(chord(qa, qb, 0) * u) * qa;
                prop_assert!(rotation_angle(q, want) < 1e-12, "segment {i} at {u}");
            }
        }
    }

    #[test]
    fn level_keys_keep_level(seeds in prop::collection::vec(seed(true), 2..=12)) {
        let v = build(&seeds, OrientMode::Level, DVec3::Y, false);
        let ks = keys(&v);
        for i in 0..ks.len() - 1 {
            let pitch = |k: &Key<Orient>| match k.value {
                Orient::View(o) => o.ypr().pitch,
                Orient::Aim { .. } => unreachable!(),
            };
            let (lo, hi) = (pitch(&ks[i]).min(pitch(&ks[i + 1])), pitch(&ks[i]).max(pitch(&ks[i + 1])));
            for (t, q) in samples(&v, &[], i, 500) {
                let y = Ypr::from_quat(q);
                prop_assert!(y.roll.abs() < 1e-12, "roll {} at {t}", y.roll);
                prop_assert!(y.pitch >= lo - 1e-12 && y.pitch <= hi + 1e-12, "pitch {} at {t}", y.pitch);
            }
        }
    }

    #[test]
    fn a_small_move_of_a_key_moves_the_path_a_little(
        bends in prop::collection::vec((any::<bool>(), 5.0f64..90.0, any::<bool>(), prop_oneof![Just(0.0), -0.02f64..0.02], -30.0f64..30.0, ease()), 2..=8),
        seeds in prop::collection::vec(seed(false), 2..=8),
        bent in any::<bool>(),
        mode in mode(),
        pick in any::<prop::sample::Index>(),
        gaps in prop::collection::vec(100i64..=2000, 8),
        axis in (-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0),
    ) {
        // Either random keys (rolls kept within ±60°, where the shortest
        // way for roll is far from switching sides), or pans and tilts in
        // turn, whose chords meet at a right angle or nearly: where a
        // limit on the velocity jumps (Free's did, at a right angle).
        let seeds: Vec<Seed> = if bent {
            let (mut yaw, mut pitch) = (0.0f64, 0.0f64);
            bends
                .iter()
                .zip(&gaps)
                .map(|(&(pan, size, back, noise, roll, ease), &gap_ms)| {
                    let sign = if back { -1.0 } else { 1.0 };
                    if pan {
                        yaw += sign * size;
                        pitch += noise;
                    } else {
                        pitch = (pitch + sign * size.min(40.0)).clamp(-80.0, 80.0);
                        yaw += noise;
                    }
                    // Roll only where the chords are bent anyway.
                    let roll = if noise == 0.0 { 0.0 } else { roll };
                    Seed { gap_ms, yaw, pitch, roll, far: None, ease, spin: 0, aim: false }
                })
                .collect()
        } else {
            seeds.into_iter().map(|s| Seed { roll: s.roll / 3.0, ..s }).collect()
        };
        let axis = DVec3::from(axis).try_normalize().unwrap_or(DVec3::Y);
        let before = build(&seeds, mode, DVec3::Y, false);
        let mut after = before.clone();
        let b = pick.index(seeds.len());
        let nudge = 1e-3f64.to_radians();
        let k = &mut after.orientation.keys[b];
        let Orient::View(o) = k.value else { unreachable!() };
        k.value = Orient::View(Orientation::new(exp(axis * nudge) * o.quat()).unwrap());
        for (id, spin) in follow_spin(&before, &after, &[]) {
            after.orientation.keys.iter_mut().find(|k| k.id == id).unwrap().spin = spin;
        }
        let ks = keys(&before);
        let end = secs(&ks[ks.len() - 1]);
        let times = (0..=2000).map(|j| end * f64::from(j) / 2000.0).chain(ks.iter().map(secs));
        for t in times {
            let moved = rotation_angle(before.orientation_at(t, &[]), after.orientation_at(t, &[]));
            prop_assert!(moved <= 50.0 * nudge, "key {b} moved by 0.001° moves the view {}° at {t}", moved.to_degrees());
        }
    }

    #[test]
    fn sticky_spin_moves_nothing_far(
        seeds in prop::collection::vec(seed(false), 2..=8),
        mode in mode(),
        pick in any::<prop::sample::Index>(),
        axis in (-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0),
    ) {
        let axis = DVec3::from(axis).try_normalize().unwrap_or(DVec3::Y);
        let base = build(&seeds, mode, axis, false);
        let b = 1 + pick.index(seeds.len() - 1);
        let near = |turn_deg: f64| {
            let mut v = base.clone();
            let Orient::View(a) = v.orientation.keys[b - 1].value else { unreachable!() };
            let Orient::View(old) = v.orientation.keys[b].value else { unreachable!() };
            let o = match mode {
                OrientMode::Level => {
                    let (pa, pb) = (a.ypr(), old.ypr());
                    Orientation::from_ypr(Ypr::new(pa.yaw + turn_deg.to_radians(), pb.pitch, pb.roll))
                }
                OrientMode::Free => Orientation::new(exp(axis * turn_deg.to_radians()) * a.quat()),
            };
            v.orientation.keys[b].value = Orient::View(o.unwrap());
            v
        };
        let before = near(179.995);
        let mut after = near(180.005);
        for (id, spin) in follow_spin(&before, &after, &[]) {
            let k = after.orientation.keys.iter_mut().find(|k| k.id == id).unwrap();
            k.spin = spin;
        }
        let end = secs(&keys(&base)[seeds.len() - 1]);
        for j in 0..=2000 {
            let t = end * f64::from(j) / 2000.0;
            let moved = rotation_angle(before.orientation_at(t, &[]), after.orientation_at(t, &[]));
            prop_assert!(moved < 1f64.to_radians(), "moved {}° at {t}", moved.to_degrees());
        }
    }

    #[test]
    fn aim_keys_follow_their_target_and_join_smoothly(
        seeds in prop::collection::vec(seed(false), 2..=10),
        mode in mode(),
        path in path(),
    ) {
        let end_ms: i64 = seeds.iter().map(|s| s.gap_ms).sum();
        let targets = [target(&path, end_ms)];
        let v = build(&seeds, mode, DVec3::Y, true);
        let ks = keys(&v);
        let dir = |q: DQuat| q * DVec3::NEG_Z;
        let aimed = |t: f64| {
            let a = targets[0].at(t);
            dir(Ypr::new(a.yaw, a.pitch, 0.0).to_quat())
        };
        for (i, k) in ks.iter().enumerate() {
            if let Orient::Aim { .. } = k.value {
                let t = secs(k);
                prop_assert!(angle_between(dir(v.orientation_at(t, &targets)), aimed(t)) < 1e-9, "key {i}");
            }
        }
        for i in 0..ks.len() - 1 {
            let both = matches!((ks[i].value, ks[i + 1].value), (Orient::Aim { .. }, Orient::Aim { .. }));
            if !both {
                continue;
            }
            for (t, q) in samples(&v, &targets, i, 200) {
                prop_assert!(angle_between(dir(q), aimed(t)) < 1e-9, "segment {i} at {t}");
            }
        }
        check_c1(&v, &targets)?;
    }

    #[test]
    fn evaluation_survives_save_and_load(
        seeds in prop::collection::vec(seed(false), 2..=12),
        mode in mode(),
        path in path(),
        fovs in prop::collection::vec((10.0f64..360.0, ease()), 1..=4),
    ) {
        // Keys within the 10 s clip.
        let mut seeds = seeds;
        for s in &mut seeds {
            s.gap_ms = s.gap_ms * 4 / 10;
            s.spin = s.spin.clamp(-1, 1);
        }
        let end_ms: i64 = seeds.iter().map(|s| s.gap_ms).sum();
        let mut p = common::project();
        let clip = common::clip(&p);
        let mut t = target(&path, end_ms);
        t.id = TargetId::NEW;
        for k in &mut t.path.keys {
            k.id = KeyId::NEW;
        }
        p.apply(&[Edit::AddTarget { target: t }]).unwrap();
        let target_id = p.targets()[0].id;
        let mut v = build(&seeds, mode, DVec3::Y, true);
        for k in &mut v.orientation.keys {
            k.id = KeyId::NEW;
            if let Orient::Aim { target, .. } = &mut k.value {
                *target = target_id;
            }
        }
        let fov: Vec<Key<f64>> = fovs
            .iter()
            .enumerate()
            .map(|(i, &(deg, ease))| Key { id: KeyId::NEW, at: ms(i as i64 * 2000), value: deg, ease, spin: 0 })
            .collect();
        let edits = [
            Edit::SetKeys { clip, keys: AnyKeys::Orientation(v.orientation.keys) },
            Edit::SetKeys { clip, keys: AnyKeys::Fov(fov) },
            Edit::SetOrientMode { clip, mode },
        ];
        p.apply(&edits).unwrap();
        let folder = Path::new("/projects");
        let back = file::from_json(&file::to_json(&p, folder).unwrap(), folder).unwrap();
        let (a, b) = (&p.the_clip().view, &back.the_clip().view);
        for j in 0..=1000 {
            let t = 10.0 * f64::from(j) / 1000.0;
            let (x, y) = (a.at(t, p.targets()), b.at(t, back.targets()));
            prop_assert_eq!(x.orientation.to_array().map(f64::to_bits), y.orientation.to_array().map(f64::to_bits));
            prop_assert_eq!(x.lens.hfov.to_bits(), y.lens.hfov.to_bits());
            prop_assert_eq!(x.lens.morph.to_bits(), y.lens.morph.to_bits());
        }
    }
}
