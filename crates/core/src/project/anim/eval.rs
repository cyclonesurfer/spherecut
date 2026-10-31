//! The view between keys (M2 plan sections 6.2 to 6.5, DECISIONS D50),
//! from `spherecut_sphere::spline`'s curves.
//!
//! Every track holds its first key's value before it and its last key's
//! after it. An aim key holds its aim rather than a direction: before the
//! first key, after the last and while it holds, it follows its target.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use glam::DQuat;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, LensShape, Projection};
use spherecut_sphere::spline::{
    Knot, RotKnot, Window, chord, follow_spin_angle, follow_spin_rotation, locate, slope_at_from,
    slope_at_to, spun, wrap,
};

use super::{Key, Orient, OrientMode, Target, ViewAnim};
use crate::project::limits::MAX_SPIN;
use crate::project::{KeyId, TargetId};

/// How far up or down a view aims at a target, in degrees: a level view
/// of the zenith has no heading.
pub const MAX_AIM_PITCH_DEG: f64 = 89.0;

/// A view at one time: where it looks (D33: `world = q · view`) and
/// through which lens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub orientation: DQuat,
    pub lens: Lens,
}

/// Where a target is at one time, as the yaw and pitch of a level view
/// looking at it (radians; pitch within ±[`MAX_AIM_PITCH_DEG`]), and
/// how fast they change (radians per second).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Aim {
    pub yaw: f64,
    pub pitch: f64,
    pub yaw_rate: f64,
    pub pitch_rate: f64,
}

impl ViewAnim {
    /// The view at `t`, seconds of the clip's source time. `targets` are
    /// the project's, in id order; an aim key at a target that isn't
    /// there (which `validate` refuses) looks straight ahead.
    pub fn at(&self, t: f64, targets: &[Target]) -> View {
        View {
            orientation: self.orientation_at(t, targets),
            lens: self.lens_at(t),
        }
    }

    /// The view's orientation at `t`.
    pub fn orientation_at(&self, t: f64, targets: &[Target]) -> DQuat {
        let w = match segment(&self.orientation.keys, t) {
            None => return DQuat::IDENTITY,
            Some(Err(k)) => return held(k, t, targets),
            Some(Ok(w)) => w,
        };
        if w.from.ease.hold {
            return held(w.from, t, targets);
        }
        if let Some(target) = same_target(w.from, w.to) {
            return follow(&w, t, aim_of(targets, target, t));
        }
        let keys = w.map(|k| (k, KeyView::of(k, targets)));
        match self.mode {
            OrientMode::Level => level(&keys, t),
            OrientMode::Free => free(&keys, t),
        }
    }

    /// The horizontal field of view at `t`, in radians: the spline runs
    /// through the keys' logarithms, so a zoom is even in apparent size.
    pub fn fov_at(&self, t: f64) -> f64 {
        let ln = match segment(&self.fov.keys, t) {
            None => return 90f64.to_radians(),
            Some(Err(k)) => k.value.ln(),
            Some(Ok(w)) => w.map(|k| knot(k, k.value.ln())).value(t),
        };
        ln.exp().to_radians()
    }

    /// The lens at `t`. Each key's shape is a place on a loop through the
    /// three projections ([`LensShape::perimeter`]), and the track is a
    /// scalar spline of it the shortest way round, so a change of
    /// projection is a morph, shaped by the keys' eases (Stop–Stop, the
    /// default, is smoothstep), and keys in a row morph on through.
    pub fn lens_at(&self, t: f64) -> Lens {
        let hfov = self.fov_at(t);
        let shape = match segment(&self.projection.keys, t) {
            None => LensShape::Pure(Projection::Rectilinear),
            Some(Err(k)) => k.value,
            // Between two keys of one shape the clamp holds it still.
            Some(Ok(w)) if w.from.ease.hold || w.from.value == w.to.value => w.from.value,
            Some(Ok(w)) => {
                let loop_ = w
                    .map(|k| knot(k, k.value.perimeter()))
                    .unwrap_periodic(LensShape::PERIMETER, [0; 3]);
                let c = loop_.value(t);
                // A pure projection where rounding leaves a trace of morph.
                let near = c.round();
                LensShape::from_perimeter(if (c - near).abs() < 1e-12 { near } else { c })
            }
        };
        shape.lens(hfov)
    }
}

impl Target {
    /// Where the target is at `t`: its path's yaw (the shortest way
    /// between keys) and pitch through the scalar spline, the pitch
    /// clamped for aiming.
    pub fn at(&self, t: f64) -> Aim {
        let d = f64::to_radians;
        let (yaw, pitch, yaw_rate, pitch_rate) = match segment(&self.path.keys, t) {
            None => (0.0, 0.0, 0.0, 0.0),
            Some(Err(k)) => (d(k.value.yaw_deg), d(k.value.pitch_deg), 0.0, 0.0),
            Some(Ok(w)) => {
                let yaw = w.map(|k| knot(k, d(k.value.yaw_deg))).unwrap_angles([0; 3]);
                let pitch = w.map(|k| knot(k, d(k.value.pitch_deg)));
                (yaw.value(t), pitch.value(t), yaw.slope(t), pitch.slope(t))
            }
        };
        let max = d(MAX_AIM_PITCH_DEG);
        let (pitch, pitch_rate) = if pitch.abs() > max {
            (pitch.clamp(-max, max), 0.0)
        } else {
            (pitch, pitch_rate)
        };
        Aim {
            yaw: wrap(yaw),
            pitch,
            yaw_rate,
            pitch_rate,
        }
    }
}

/// The spins that keep `next`'s orientation segments turning the way
/// they did in `previous` (M2 plan section 6.4's sticky spin): for each
/// segment whose two keys are next to each other in both and changed,
/// the spin of its first key that makes its turn the nearest to the turn
/// it made before, if that differs from the spin it has. A drag adds
/// these as `set_key` edits each frame, so a key carried past 180° from
/// its neighbour keeps the path going the way it went.
pub fn follow_spin(previous: &ViewAnim, next: &ViewAnim, targets: &[Target]) -> Vec<(KeyId, i8)> {
    let mut spins = Vec::new();
    if previous.mode != next.mode {
        return spins;
    }
    let before = &previous.orientation.keys;
    let index: HashMap<KeyId, usize> = before.iter().enumerate().map(|(i, k)| (k.id, i)).collect();
    for pair in next.orientation.keys.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let Some(&i) = index.get(&a.id) else {
            continue;
        };
        let Some(pb) = before.get(i + 1).filter(|k| k.id == b.id) else {
            continue;
        };
        let pa = &before[i];
        if (pa, pb) == (a, b) || same_target(a, b).is_some() {
            continue;
        }
        let view = |k: &Key<Orient>| KeyView::of(k, targets);
        let spin = match next.mode {
            OrientMode::Level => {
                let (ya, yb) = (view(pa).ypr.yaw, view(pb).ypr.yaw);
                let turn = spun(wrap(yb - ya), pa.spin, TAU);
                follow_spin_angle(turn, view(a).ypr.yaw, view(b).ypr.yaw)
            }
            OrientMode::Free => {
                let turn = chord(view(pa).q, view(pb).q, pa.spin);
                follow_spin_rotation(turn, view(a).q, view(b).q)
            }
        };
        let spin = spin.clamp(-MAX_SPIN, MAX_SPIN);
        if spin != a.spin {
            spins.push((a.id, spin));
        }
    }
    spins
}

fn secs<V>(k: &Key<V>) -> f64 {
    k.at.to_secs_f64()
}

fn knot<V>(k: &Key<V>, v: f64) -> Knot {
    Knot {
        t: secs(k),
        v,
        ease: k.ease,
        rate: None,
    }
}

/// The keys around `t`: `Err` with the key whose value holds there
/// (at or outside the track's ends), or `Ok` with the segment's window.
/// None for a track with no keys, which `validate` refuses.
type Segment<'a, V> = Option<Result<Window<&'a Key<V>>, &'a Key<V>>>;

fn segment<V>(keys: &[Key<V>], t: f64) -> Segment<'_, V> {
    if keys.is_empty() {
        return None;
    }
    Some(match locate(|i| secs(&keys[i]), keys.len(), t) {
        Err(i) => Err(&keys[i]),
        Ok(i) => Ok(Window {
            before: i.checked_sub(1).map(|j| &keys[j]),
            from: &keys[i],
            to: &keys[i + 1],
            after: keys.get(i + 2),
        }),
    })
}

fn aim_of(targets: &[Target], id: TargetId, t: f64) -> Aim {
    targets
        .binary_search_by_key(&id, |t| t.id)
        .map(|i| targets[i].at(t))
        .unwrap_or_default()
}

/// The target both keys aim at, if they aim at the same one: the
/// segment between them follows it.
fn same_target(a: &Key<Orient>, b: &Key<Orient>) -> Option<TargetId> {
    match (a.value, b.value) {
        (Orient::Aim { target: x, .. }, Orient::Aim { target: y, .. }) if x == y => Some(x),
        _ => None,
    }
}

/// What a key holds while it holds: its orientation, or its aim at `t`.
fn held(k: &Key<Orient>, t: f64, targets: &[Target]) -> DQuat {
    match k.value {
        Orient::View(o) => o.quat(),
        Orient::Aim { target, roll_deg } => {
            let a = aim_of(targets, target, t);
            Ypr::new(a.yaw, a.pitch, roll_deg.to_radians()).to_quat()
        }
    }
}

fn roll_of(k: &Key<Orient>) -> f64 {
    match k.value {
        Orient::View(o) => o.ypr().roll,
        Orient::Aim { roll_deg, .. } => roll_deg.to_radians(),
    }
}

/// The segment between two keys aimed at the same target: the target's
/// direction, with the roll through the scalar spline.
fn follow(w: &Window<&Key<Orient>>, t: f64, aim: Aim) -> DQuat {
    let roll = w.map(|k| knot(k, roll_of(k))).unwrap_angles([0; 3]);
    Ypr::new(aim.yaw, aim.pitch, roll.value(t)).to_quat()
}

/// An orientation key at its own time.
#[derive(Clone, Copy)]
struct KeyView {
    ypr: Ypr,
    q: DQuat,
    /// For an aim key, its target there.
    aim: Option<Aim>,
}

impl KeyView {
    fn of(k: &Key<Orient>, targets: &[Target]) -> KeyView {
        match k.value {
            Orient::View(o) => KeyView {
                ypr: o.ypr(),
                q: o.quat(),
                aim: None,
            },
            Orient::Aim { target, roll_deg } => {
                let a = aim_of(targets, target, secs(k));
                let ypr = Ypr::new(a.yaw, a.pitch, roll_deg.to_radians());
                KeyView {
                    ypr,
                    q: ypr.to_quat(),
                    aim: Some(a),
                }
            }
        }
    }
}

type Keys<'a> = Window<(&'a Key<Orient>, KeyView)>;

/// Level: yaw, pitch and roll as numbers, yaw the shortest way plus each
/// segment's spin, roll the shortest way. An aim key's yaw and pitch
/// move at its target's rate on a `Smooth` side.
fn level(w: &Keys<'_>, t: f64) -> DQuat {
    let spins = [
        w.before.map_or(0, |(k, _)| k.spin),
        w.from.0.spin,
        w.to.0.spin,
    ];
    let yaw = w
        .map(|(k, v)| Knot {
            rate: v.aim.map(|a| a.yaw_rate),
            ..knot(k, v.ypr.yaw)
        })
        .unwrap_angles(spins);
    let pitch = w.map(|(k, v)| Knot {
        rate: v.aim.map(|a| a.pitch_rate),
        ..knot(k, v.ypr.pitch)
    });
    let roll = w.map(|(k, v)| knot(k, v.ypr.roll)).unwrap_angles([0; 3]);
    // An aim key's pitch rate isn't limited as the spline's own slopes
    // are, so beside a target leaving the zenith fast the pitch can pass
    // ±90°: held there, the view never tips over the top.
    let pitch = pitch.value(t).clamp(-FRAC_PI_2, FRAC_PI_2);
    Ypr::new(yaw.value(t), pitch, roll.value(t)).to_quat()
}

/// Free: Kim–Kim–Shin on SO(3). An aim key's angular velocity on a
/// `Smooth` side is its target's turn with the roll's spline slope, so a
/// segment joins one that follows the target without a jolt.
fn free(w: &Keys<'_>, t: f64) -> DQuat {
    let roll = w.map(|(k, v)| knot(k, v.ypr.roll)).unwrap_angles([0; 3]);
    let omega = |v: KeyView, roll_rate: Option<f64>| {
        v.aim.map(|a| {
            v.ypr
                .omega(Ypr::new(a.yaw_rate, a.pitch_rate, roll_rate.unwrap_or(0.0)))
        })
    };
    let rot = |(k, v): (&Key<Orient>, KeyView)| RotKnot {
        t: secs(k),
        q: v.q,
        ease: k.ease,
        spin: k.spin,
        omega: None,
    };
    let mut r = w.map(rot);
    r.from.omega = omega(w.from.1, slope_at_from(&roll));
    r.to.omega = omega(w.to.1, slope_at_to(&roll));
    r.value(t)
}
