//! Curves through keys (M2 plan section 6, DECISIONS D50): the pieces a
//! view's tracks are evaluated with, in `f64` on the CPU.
//!
//! * A **scalar** track (yaw, pitch and roll in Level mode, a field of
//!   view in log space, a target's path, a projection morph) is a cubic
//!   Hermite per segment. A key's slope is the non-uniform Catmull–Rom
//!   one (the slope at the key of the parabola through it and its
//!   neighbours), clamped as Fritsch and Carlson do: 0 where the chords
//!   on either side turn back, and at most 3× the smaller chord's, so
//!   monotone keys give a monotone curve that never overshoots them.
//! * A **rotation** track (Free mode) is Kim, Kim and Shin's cumulative
//!   cubic Hermite on SO(3) (SIGGRAPH 1995), with angular velocities
//!   chosen and clamped the same way, so it passes through every key and
//!   is C¹ wherever a key's two sides match, at any spacing of the keys
//!   (squad is C¹ only when they are evenly spaced).
//! * Each key's **ease** says, per side, which velocity it has there:
//!   the spline's (`Smooth`), the adjacent segment's average
//!   (`Linear`), or none (`Stop`); `hold` keeps its value until the next
//!   key. A key with no neighbour on a side has no spline velocity
//!   there, so at a track's first and last key `Smooth` is `Stop`, unless
//!   the key knows its own velocity (a key aimed at a moving target).
//!
//! Evaluation is local: a segment needs only its two keys and one more on
//! each side ([`Window`]), so a lookup is a binary search ([`locate`]) and
//! a constant amount of work whatever the number of keys.

use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;

/// One side of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The spline's velocity at the key.
    #[default]
    Smooth,
    /// The adjacent segment's average rate.
    Linear,
    /// At rest.
    Stop,
}

/// How a key is entered and left (M2 plan section 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ease {
    #[serde(default)]
    pub into: Side,
    #[serde(default)]
    pub out: Side,
    /// Keep this key's value until the next key, then jump.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hold: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Ease {
    /// Passes through without a jolt.
    pub const SMOOTH: Ease = Ease::sides(Side::Smooth, Side::Smooth);
    /// Arrives, stops, leaves from rest.
    pub const STOP: Ease = Ease::sides(Side::Stop, Side::Stop);
    /// Constant speed on each side.
    pub const LINEAR: Ease = Ease::sides(Side::Linear, Side::Linear);
    /// Arrives slowly: at rest on the way in.
    pub const ARRIVE: Ease = Ease::sides(Side::Stop, Side::Smooth);
    /// Leaves slowly: at rest on the way out.
    pub const LEAVE: Ease = Ease::sides(Side::Smooth, Side::Stop);
    /// Keeps the value until the next key.
    pub const HOLD: Ease = Ease {
        into: Side::Smooth,
        out: Side::Smooth,
        hold: true,
    };

    pub const fn sides(into: Side, out: Side) -> Ease {
        Ease {
            into,
            out,
            hold: false,
        }
    }
}

/// The velocity a side asks for: `smooth` is the spline's (none where
/// the key has no neighbour on that side), `chord` the adjacent
/// segment's average.
pub fn side_velocity<V: Default>(side: Side, smooth: Option<V>, chord: V) -> V {
    match side {
        Side::Stop => V::default(),
        Side::Linear => chord,
        Side::Smooth => smooth.unwrap_or_default(),
    }
}

/// The smooth slope at a key at `t1` with value `v1`, between keys at
/// (`t0`, `v0`) and (`t2`, `v2`): non-uniform Catmull–Rom, clamped as
/// Fritsch and Carlson do.
pub fn smooth_slope((t0, v0): (f64, f64), (t1, v1): (f64, f64), (t2, v2): (f64, f64)) -> f64 {
    let (h0, h1) = (t1 - t0, t2 - t1);
    let (d0, d1) = ((v1 - v0) / h0, (v2 - v1) / h1);
    if d0 * d1 <= 0.0 {
        return 0.0;
    }
    let m = (h1 * d0 + h0 * d1) / (h0 + h1);
    let cap = 3.0 * d0.abs().min(d1.abs());
    m.clamp(-cap, cap)
}

/// The cubic Hermite from (`t0`, `v0`) with slope `m0` to (`t1`, `v1`)
/// with slope `m1`, at `t` (clamped to the segment).
pub fn hermite(t0: f64, v0: f64, m0: f64, t1: f64, v1: f64, m1: f64, t: f64) -> f64 {
    let h = t1 - t0;
    let u = ((t - t0) / h).clamp(0.0, 1.0);
    let (u2, u3) = (u * u, u * u * u);
    let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
    let h10 = u3 - 2.0 * u2 + u;
    let h01 = -2.0 * u3 + 3.0 * u2;
    let h11 = u3 - u2;
    h00 * v0 + h10 * h * m0 + h01 * v1 + h11 * h * m1
}

/// The slope of [`hermite`] at `t`.
pub fn hermite_slope(t0: f64, v0: f64, m0: f64, t1: f64, v1: f64, m1: f64, t: f64) -> f64 {
    let h = t1 - t0;
    let u = ((t - t0) / h).clamp(0.0, 1.0);
    let u2 = u * u;
    let d00 = 6.0 * u2 - 6.0 * u;
    let d10 = 3.0 * u2 - 4.0 * u + 1.0;
    let d01 = -6.0 * u2 + 6.0 * u;
    let d11 = 3.0 * u2 - 2.0 * u;
    (d00 * v0 + d01 * v1) / h + d10 * m0 + d11 * m1
}

/// A segment's two keys and their neighbours, if any.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window<K> {
    pub before: Option<K>,
    pub from: K,
    pub to: K,
    pub after: Option<K>,
}

impl<K> Window<K> {
    pub fn map<L>(self, mut f: impl FnMut(K) -> L) -> Window<L> {
        Window {
            before: self.before.map(&mut f),
            from: f(self.from),
            to: f(self.to),
            after: self.after.map(&mut f),
        }
    }
}

/// A key of a scalar track: its time in seconds, value and ease, and
/// its own velocity if it has one (an aim key's, from its target), which
/// its `Smooth` sides then take instead of the spline's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Knot {
    pub t: f64,
    pub v: f64,
    pub ease: Ease,
    pub rate: Option<f64>,
}

impl Window<Knot> {
    /// The window of an angle track, each segment the shortest way plus
    /// its `turns` (whole turns, for the segments before → from,
    /// from → to and to → after, as [`spun`] adds them): values are
    /// unwrapped from `from`'s, so `to` may be more than π from it.
    pub fn unwrap_angles(self, turns: [i8; 3]) -> Window<Knot> {
        self.unwrap_periodic(TAU, turns)
    }

    /// [`Window::unwrap_angles`] for values that repeat every `period`.
    pub fn unwrap_periodic(mut self, period: f64, turns: [i8; 3]) -> Window<Knot> {
        let way = |a: f64, b: f64, n: i8| spun(wrap_period(b - a, period), n, period);
        let from = self.from.v;
        if let Some(b) = &mut self.before {
            b.v = from - way(b.v, from, turns[0]);
        }
        let to = from + way(from, self.to.v, turns[1]);
        if let Some(a) = &mut self.after {
            a.v = to + way(self.to.v, a.v, turns[2]);
        }
        self.to.v = to;
        self
    }

    /// The segment's average rate.
    pub fn chord(&self) -> f64 {
        (self.to.v - self.from.v) / (self.to.t - self.from.t)
    }

    /// The slopes leaving `from` and entering `to`.
    pub fn slopes(&self) -> (f64, f64) {
        let (a, b) = (self.from, self.to);
        let chord = self.chord();
        let at = |k: Knot| (k.t, k.v);
        let out = side_velocity(
            a.ease.out,
            a.rate
                .or_else(|| self.before.map(|p| smooth_slope(at(p), at(a), at(b)))),
            chord,
        );
        let into = side_velocity(
            b.ease.into,
            b.rate
                .or_else(|| self.after.map(|n| smooth_slope(at(a), at(b), at(n)))),
            chord,
        );
        (out, into)
    }

    /// The track's value at `t` within the segment.
    pub fn value(&self, t: f64) -> f64 {
        let (a, b) = (self.from, self.to);
        if a.ease.hold {
            return if t < b.t { a.v } else { b.v };
        }
        let (m0, m1) = self.slopes();
        hermite(a.t, a.v, m0, b.t, b.v, m1, t)
    }

    /// The track's slope at `t` within the segment.
    pub fn slope(&self, t: f64) -> f64 {
        let (a, b) = (self.from, self.to);
        if a.ease.hold {
            return 0.0;
        }
        let (m0, m1) = self.slopes();
        hermite_slope(a.t, a.v, m0, b.t, b.v, m1, t)
    }
}

/// The smooth slope at the key `from` of a window, if it has a key
/// before it (for a key's own rate, as [`Window::slopes`] would give it).
pub fn slope_at_from(w: &Window<Knot>) -> Option<f64> {
    let at = |k: Knot| (k.t, k.v);
    w.before.map(|p| smooth_slope(at(p), at(w.from), at(w.to)))
}

/// The smooth slope at the key `to` of a window, if it has a key after
/// it.
pub fn slope_at_to(w: &Window<Knot>) -> Option<f64> {
    let at = |k: Knot| (k.t, k.v);
    w.after.map(|n| smooth_slope(at(w.from), at(w.to), at(n)))
}

/// A key of a rotation track: its time in seconds, the rotation, ease,
/// the extra whole turns of the segment that leaves it, and its own
/// world-frame angular velocity if it has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotKnot {
    pub t: f64,
    pub q: DQuat,
    pub ease: Ease,
    pub spin: i8,
    pub omega: Option<DVec3>,
}

impl Window<RotKnot> {
    /// The world-frame rotation vector of the segment, spin included.
    pub fn chord(&self) -> DVec3 {
        chord(self.from.q, self.to.q, self.from.spin)
    }

    /// The world-frame angular velocities leaving `from` and entering
    /// `to`.
    pub fn velocities(&self) -> (DVec3, DVec3) {
        let (a, b) = (self.from, self.to);
        let h = b.t - a.t;
        let d = self.chord();
        let out = side_velocity(
            a.ease.out,
            a.omega.or_else(|| {
                self.before
                    .map(|p| smooth_omega(chord(p.q, a.q, p.spin), a.t - p.t, d, h))
            }),
            d / h,
        );
        let into = side_velocity(
            b.ease.into,
            b.omega.or_else(|| {
                self.after
                    .map(|n| smooth_omega(d, h, chord(b.q, n.q, b.spin), n.t - b.t))
            }),
            d / h,
        );
        (out, into)
    }

    /// The rotation at `t` within the segment.
    pub fn value(&self, t: f64) -> DQuat {
        let (a, b) = (self.from, self.to);
        if a.ease.hold {
            return if t < b.t { a.q } else { b.q };
        }
        let h = b.t - a.t;
        let (w0, w1) = self.velocities();
        rotation_hermite(a.q, b.q, self.chord(), w0, w1, h, (t - a.t) / h)
    }
}

/// Where `t` falls among `len` keys at increasing `times` (at least
/// one): `Err(i)` at or before the first key (i = 0) or at or after the
/// last (i = last); `Ok(i)` within the segment from key i to i + 1, key
/// i's time included.
pub fn locate(times: impl Fn(usize) -> f64, len: usize, t: f64) -> Result<usize, usize> {
    debug_assert!(len > 0);
    if len == 1 || t <= times(0) {
        return Err(0);
    }
    if t >= times(len - 1) {
        return Err(len - 1);
    }
    // The last key at or before t.
    let (mut lo, mut hi) = (0, len - 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if times(mid) <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

/// An angle in (−π, π].
pub fn wrap(a: f64) -> f64 {
    wrap_period(a, TAU)
}

/// `a` plus or minus whole `period`s, in (−period/2, period/2].
pub fn wrap_period(a: f64, period: f64) -> f64 {
    let half = period / 2.0;
    let w = (a + half).rem_euclid(period) - half;
    if w <= -half { w + period } else { w }
}

/// The rotation vector of `q` (axis times angle, the angle in [0, π]),
/// in the frame `q` is written in.
pub fn log(q: DQuat) -> DVec3 {
    let q = if q.w < 0.0 { -q } else { q };
    let v = q.xyz();
    let s = v.length();
    if s < 1e-300 {
        return DVec3::ZERO;
    }
    let angle = 2.0 * s.atan2(q.w);
    v * (angle / s)
}

/// The rotation by the rotation vector `v`.
pub fn exp(v: DVec3) -> DQuat {
    let angle = v.length();
    if angle < 1e-300 {
        return DQuat::IDENTITY;
    }
    DQuat::from_axis_angle(v / angle, angle)
}

/// A segment's turn: the short way `short` plus `spin` whole turns of
/// `period` the way it goes (the way of increasing values, a yaw to the
/// right, when it is 0), so a negative spin goes the other way round
/// and −1 is the long way. Level's yaw and Free's rotation ([`chord`])
/// count spin alike: keys that differ only in yaw turn the same in both.
pub fn spun(short: f64, spin: i8, period: f64) -> f64 {
    short + way(short) * period * f64::from(spin)
}

/// The sign of a short way's turn: none (under the 1e−12 [`chord`]
/// takes as none) counts as increasing.
fn way(short: f64) -> f64 {
    if short < -SAME { -1.0 } else { 1.0 }
}

/// A rotation or turn this small (radians) is none, for which way whole
/// turns go.
const SAME: f64 = 1e-12;

/// The world-frame rotation vector from `a` to `b` (`b = exp(δ) · a`):
/// the shortest way, plus `spin` whole turns about its axis (about −Y,
/// a turn to the right, when `a` and `b` are the same). A negative spin
/// goes the other way round: −1 is the long way.
pub fn chord(a: DQuat, b: DQuat, spin: i8) -> DVec3 {
    let d = log(b * a.conjugate());
    if spin == 0 {
        return d;
    }
    d + turn_axis(d) * (TAU * f64::from(spin))
}

/// The axis whole turns are added about for a short-way rotation `d`:
/// its own, or with none −Y, about which a yaw to the right turns (D33's
/// yaw is `R_y(−yaw)`), as Level's spin turns equal keys.
fn turn_axis(d: DVec3) -> DVec3 {
    let len = d.length();
    if len > SAME { d / len } else { DVec3::NEG_Y }
}

/// The smooth angular velocity at a key between chords `d0` (over `h0`
/// seconds, into the key) and `d1` (over `h1`, out of it), limited as
/// [`smooth_slope`] is, but continuously, so a small move of a key never
/// makes a large change of the path: scaled by the cosine of the angle
/// between the chords (none where they turn back or turn at a right
/// angle, the full Catmull–Rom velocity where they go on straight), and
/// at most 3× the smaller chord's rate.
pub fn smooth_omega(d0: DVec3, h0: f64, d1: DVec3, h1: f64) -> DVec3 {
    let (r0, r1) = (d0 / h0, d1 / h1);
    let lengths = r0.length() * r1.length();
    let cos = if lengths > 0.0 {
        r0.dot(r1) / lengths
    } else {
        0.0
    };
    if cos <= 0.0 {
        return DVec3::ZERO;
    }
    let w = (r0 * h1 + r1 * h0) * (cos / (h0 + h1));
    let cap = 3.0 * r0.length().min(r1.length());
    let len = w.length();
    if len > cap { w * (cap / len) } else { w }
}

/// Kim–Kim–Shin's cumulative cubic Hermite from `q0` to `q1` over `h`
/// seconds, with world-frame angular velocities `w0` leaving `q0` and
/// `w1` entering `q1`, and `delta` the world-frame chord (from
/// [`chord`], which carries any spin), at `u` in [0, 1].
///
/// The middle turn is the chord less the ends' (`M`, whole turns and
/// all), then a correction `R` to meet `q1`: the rotation from what the
/// three turns reach to `q1`, none when they turn about one axis. Taking
/// one rotation vector for both, as Kim–Kim–Shin do, can't carry the
/// winding: where the middle turn is a whole turn or nearly, its axis
/// follows the correction's, which a tiny move of a key turns anywhere
/// (the 100,000-case property test found a 0.001° nudge moving the view
/// 0.05°).
pub fn rotation_hermite(
    q0: DQuat,
    q1: DQuat,
    delta: DVec3,
    w0: DVec3,
    w1: DVec3,
    h: f64,
    u: f64,
) -> DQuat {
    let u = u.clamp(0.0, 1.0);
    // In q0's and q1's own frames, as they are multiplied on the right.
    let a = q0.conjugate() * (w0 * (h / 3.0));
    let c = q1.conjugate() * (w1 * (h / 3.0));
    let rel = q0.conjugate() * q1;
    let m = q0.conjugate() * delta - a - c;
    let r = log(exp(-m) * exp(-a) * rel * exp(-c));
    let v = 1.0 - u;
    let b1 = 1.0 - v * v * v;
    let b2 = u * u * (3.0 - 2.0 * u);
    let b3 = u * u * u;
    (q0 * exp(a * b1) * exp(m * b2) * exp(r * b2) * exp(c * b3)).normalize()
}

/// The spin a segment of an angle track from `a` to `b` (radians) needs
/// so that its turn, the shortest way plus whole turns, is the nearest to
/// `previous`, the turn it made before: so a drag that carries a key past
/// 180° from its neighbour keeps the path going the way it went (M2 plan
/// section 6.4).
pub fn follow_spin_angle(previous: f64, a: f64, b: f64) -> i8 {
    let short = wrap(b - a);
    to_spin((previous - short) * way(short) / TAU)
}

/// [`follow_spin_angle`] for a rotation from `a` to `b`, `previous`
/// being the segment's world-frame chord before.
pub fn follow_spin_rotation(previous: DVec3, a: DQuat, b: DQuat) -> i8 {
    let short = log(b * a.conjugate());
    let s = previous.dot(turn_axis(short));
    to_spin((s - short.length()) / TAU)
}

fn to_spin(turns: f64) -> i8 {
    // In range by the clamp.
    #[allow(clippy::cast_possible_truncation)]
    let n = turns.round().clamp(-127.0, 127.0) as i8;
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::unit_quat;
    use proptest::prelude::*;
    use std::f64::consts::PI;

    fn knot(t: f64, v: f64, ease: Ease) -> Knot {
        Knot {
            t,
            v,
            ease,
            rate: None,
        }
    }

    fn window(keys: &[Knot], i: usize) -> Window<Knot> {
        Window {
            before: i.checked_sub(1).map(|j| keys[j]),
            from: keys[i],
            to: keys[i + 1],
            after: keys.get(i + 2).copied(),
        }
    }

    fn eval(keys: &[Knot], t: f64) -> f64 {
        match locate(|i| keys[i].t, keys.len(), t) {
            Err(i) => keys[i].v,
            Ok(i) => window(keys, i).value(t),
        }
    }

    #[test]
    fn hermite_meets_its_ends_and_slopes() {
        let (t0, v0, m0, t1, v1, m1) = (1.0, 2.0, -3.0, 3.5, -1.0, 0.5);
        assert_eq!(hermite(t0, v0, m0, t1, v1, m1, t0), v0);
        assert_eq!(hermite(t0, v0, m0, t1, v1, m1, t1), v1);
        assert!((hermite_slope(t0, v0, m0, t1, v1, m1, t0) - m0).abs() < 1e-12);
        assert!((hermite_slope(t0, v0, m0, t1, v1, m1, t1) - m1).abs() < 1e-12);
        let h = 1e-6;
        let t = 2.2;
        let fd = (hermite(t0, v0, m0, t1, v1, m1, t + h) - hermite(t0, v0, m0, t1, v1, m1, t - h))
            / (2.0 * h);
        assert!((fd - hermite_slope(t0, v0, m0, t1, v1, m1, t)).abs() < 1e-8);
    }

    #[test]
    fn a_plateau_holds_instead_of_overshooting() {
        // Keys 0, 90, 90, 0: the curve stays at or below 90.
        let keys: Vec<Knot> = [(0.0, 0.0), (1.0, 90.0), (2.0, 90.0), (3.0, 0.0)]
            .map(|(t, v)| knot(t, v, Ease::SMOOTH))
            .to_vec();
        for i in 0..=3000 {
            let v = eval(&keys, f64::from(i) / 1000.0);
            assert!((-1e-12..=90.0 + 1e-12).contains(&v), "{v}");
        }
        assert_eq!(eval(&keys, 1.5), 90.0);
    }

    #[test]
    fn eases_set_the_velocity_at_a_key() {
        let mk = |e: Ease| [knot(0.0, 0.0, e), knot(1.0, 1.0, e), knot(3.0, 5.0, e)];
        let slope_at = |keys: &[Knot], i: usize| window(keys, i).slopes();
        // Smooth: the Catmull–Rom slope inside, rest at the ends.
        let s = mk(Ease::SMOOTH);
        assert_eq!(slope_at(&s, 0), (0.0, (2.0 * 1.0 + 1.0 * 2.0) / 3.0));
        assert_eq!(slope_at(&s, 1).1, 0.0);
        // Linear: the chord on each side.
        let l = mk(Ease::LINEAR);
        assert_eq!(slope_at(&l, 0), (1.0, 1.0));
        assert_eq!(slope_at(&l, 1), (2.0, 2.0));
        // Stop: none.
        assert_eq!(slope_at(&mk(Ease::STOP), 0), (0.0, 0.0));
        // A key's own rate wins over the spline's, even at an end.
        let mut r = mk(Ease::SMOOTH);
        r[0].rate = Some(7.0);
        assert_eq!(slope_at(&r, 0).0, 7.0);
        // Hold keeps the value until the next key.
        let h = mk(Ease::HOLD);
        assert_eq!(eval(&h, 0.999), 0.0);
        assert_eq!(eval(&h, 1.0), 1.0);
        assert_eq!(window(&h, 0).slope(0.5), 0.0);
    }

    #[test]
    fn locate_finds_the_segment() {
        let times = [0.0, 1.0, 2.5, 4.0];
        let at = |i: usize| times[i];
        assert_eq!(locate(at, 4, -1.0), Err(0));
        assert_eq!(locate(at, 4, 0.0), Err(0));
        assert_eq!(locate(at, 4, 0.5), Ok(0));
        assert_eq!(locate(at, 4, 1.0), Ok(1));
        assert_eq!(locate(at, 4, 3.9), Ok(2));
        assert_eq!(locate(at, 4, 4.0), Err(3));
        assert_eq!(locate(at, 1, 4.0), Err(0));
    }

    #[test]
    fn angles_unwrap_the_shortest_way_plus_turns() {
        let d = f64::to_radians;
        let w = Window {
            before: Some(knot(0.0, d(170.0), Ease::SMOOTH)),
            from: knot(1.0, d(-170.0), Ease::SMOOTH),
            to: knot(2.0, d(170.0), Ease::SMOOTH),
            after: Some(knot(3.0, d(-170.0), Ease::SMOOTH)),
        };
        let u = w.unwrap_angles([0, 0, 0]);
        let deg = |k: Option<Knot>| k.map(|k| k.v.to_degrees());
        assert!((deg(u.before).unwrap() - (-190.0)).abs() < 1e-9);
        assert!((u.to.v.to_degrees() - (-190.0)).abs() < 1e-9);
        assert!((deg(u.after).unwrap() - (-170.0)).abs() < 1e-9);
        // −170° to 170° is 20° to the left: a spin of 1 turns a whole
        // turn more that way, −1 the long way round to the right.
        let more = w.unwrap_angles([0, 1, 0]);
        assert!((more.to.v.to_degrees() - (-550.0)).abs() < 1e-9);
        assert!((deg(more.after).unwrap() - (-530.0)).abs() < 1e-9);
        let long = w.unwrap_angles([0, -1, 0]);
        assert!((long.to.v.to_degrees() - 170.0).abs() < 1e-9);
        assert!((deg(long.after).unwrap() - 190.0).abs() < 1e-9);
    }

    #[test]
    fn spin_counts_the_way_the_segment_goes() {
        let d = f64::to_radians;
        assert!((spun(d(30.0), 1, TAU) - d(390.0)).abs() < 1e-12);
        assert!((spun(d(30.0), -1, TAU) - d(-330.0)).abs() < 1e-12);
        assert!((spun(d(-30.0), 1, TAU) - d(-390.0)).abs() < 1e-12);
        assert!((spun(d(-30.0), -1, TAU) - d(330.0)).abs() < 1e-12);
        // Equal ends turn to the right, as the rotation's chord does.
        assert!((spun(0.0, 2, TAU) - d(720.0)).abs() < 1e-12);
        let yaw = |deg: f64| crate::orient::Ypr::from_degrees(deg, 0.0, 0.0).to_quat();
        for (from, to) in [(0.0, 30.0), (0.0, -30.0), (10.0, 10.0), (170.0, -170.0)] {
            for n in -2..=2 {
                let level = spun(wrap(d(to) - d(from)), n, TAU);
                // A yaw of θ is a rotation of −θ about Y.
                let free = chord(yaw(from), yaw(to), n);
                assert!(
                    (free - DVec3::NEG_Y * level).length() < 1e-9,
                    "{from} to {to} spin {n}: {free} for {level}"
                );
            }
        }
    }

    #[test]
    fn log_and_exp_invert() {
        let v = DVec3::new(0.3, -1.2, 2.0);
        assert!((log(exp(v)) - v).length() < 1e-12);
        assert_eq!(log(DQuat::IDENTITY), DVec3::ZERO);
        assert_eq!(exp(DVec3::ZERO), DQuat::IDENTITY);
        // A half turn's log has length π whichever sign the quaternion has.
        let half = DQuat::from_rotation_y(PI);
        assert!((log(half).length() - PI).abs() < 1e-12);
        assert!((log(-half).length() - PI).abs() < 1e-12);
    }

    #[test]
    fn chords_carry_their_spin() {
        let a = DQuat::from_rotation_y(0.2);
        let b = DQuat::from_rotation_y(0.5);
        assert!((chord(a, b, 0) - DVec3::Y * 0.3).length() < 1e-12);
        assert!((chord(a, b, 1) - DVec3::Y * (0.3 + TAU)).length() < 1e-12);
        assert!((chord(a, b, -1) - DVec3::Y * (0.3 - TAU)).length() < 1e-12);
        // No rotation at all spins to the right, about −Y.
        assert!((chord(a, a, 2) - DVec3::NEG_Y * (2.0 * TAU)).length() < 1e-12);
    }

    #[test]
    fn follow_spin_keeps_the_way_it_went() {
        let d = f64::to_radians;
        // A turn of 179.99° to the right whose end moves to 180.01°, now
        // 179.99° to the left the short way: the long way round.
        assert_eq!(follow_spin_angle(d(179.99), 0.0, d(180.01)), -1);
        assert_eq!(follow_spin_angle(d(179.99), 0.0, d(179.98)), 0);
        assert_eq!(follow_spin_angle(d(-179.99), 0.0, d(179.99)), -1);
        // 540° before; 181° is −179° the short way, and two turns the
        // other way round, −179° + 720°, is the nearest to 540°.
        assert_eq!(follow_spin_angle(d(540.0), 0.0, d(181.0)), -2);
        // −540° before, to −181° (179° the short way): two turns more
        // to the left of the long way.
        assert_eq!(follow_spin_angle(d(-540.0), 0.0, d(-181.0)), -2);
        let a = DQuat::IDENTITY;
        let before = chord(a, DQuat::from_rotation_y(d(179.99)), 0);
        let b = DQuat::from_rotation_y(d(180.01));
        let n = follow_spin_rotation(before, a, b);
        assert_eq!(n, -1);
        assert!((chord(a, b, n) - DVec3::Y * d(180.01)).length() < 1e-9);
    }

    /// Slerp the short way, exactly (glam's lerps between close
    /// rotations).
    fn geodesic(q0: DQuat, q1: DQuat, u: f64) -> DQuat {
        exp(chord(q0, q1, 0) * u) * q0
    }

    fn rot_window(q0: DQuat, q1: DQuat, ease: Ease, spin: i8, h: f64) -> Window<RotKnot> {
        let k = |t, q| RotKnot {
            t,
            q,
            ease,
            spin,
            omega: None,
        };
        Window {
            before: None,
            from: k(0.0, q0),
            to: k(h, q1),
            after: None,
        }
    }

    #[test]
    fn a_middle_turn_of_a_whole_turn_or_so_moves_little_with_the_ends() {
        // 179° and a turn, the leaving velocity covering the 179°: the
        // middle is a whole turn and 0.1°.
        let (q0, q1) = (
            DQuat::IDENTITY,
            DQuat::from_axis_angle(DVec3::Y, 179f64.to_radians()),
        );
        let delta = chord(q0, q1, 1);
        let h = 1.3;
        let w0 = DVec3::Y * (178.9f64.to_radians() * 3.0 / h);
        let eps = 1e-7;
        for u in (0..=40).map(|i| f64::from(i) / 40.0) {
            let at = |w0| rotation_hermite(q0, q1, delta, w0, DVec3::ZERO, h, u);
            let moved = crate::rotation_angle(at(w0), at(w0 + DVec3::Z * eps));
            // The leaving turn moves by eps·h/3.
            assert!(moved < 3.0 * eps * h, "{moved:e} at {u}");
        }
        // The turn is kept: past the 179° the path goes on round.
        let mid = rotation_hermite(q0, q1, delta, w0, DVec3::ZERO, h, 0.75);
        assert!(crate::rotation_angle(mid, q1) > 1.0);
    }

    proptest! {
        #[test]
        fn linear_rotation_is_slerp(q0 in unit_quat(), q1 in unit_quat(), u in 0.0f64..=1.0, h in 0.1f64..5.0) {
            let w = rot_window(q0, q1, Ease::LINEAR, 0, h);
            let got = w.value(u * h);
            prop_assert!(crate::rotation_angle(got, geodesic(q0, q1, u)) < 1e-12);
        }

        #[test]
        fn stopped_rotation_is_slerp_at_smoothstep(q0 in unit_quat(), q1 in unit_quat(), u in 0.0f64..=1.0) {
            let w = rot_window(q0, q1, Ease::STOP, 0, 2.0);
            let s = u * u * (3.0 - 2.0 * u);
            prop_assert!(crate::rotation_angle(w.value(2.0 * u), geodesic(q0, q1, s)) < 1e-12);
        }

        #[test]
        fn a_rotation_meets_its_keys(q0 in unit_quat(), q1 in unit_quat(), spin in -3i8..=3) {
            for ease in [Ease::SMOOTH, Ease::LINEAR, Ease::STOP, Ease::ARRIVE] {
                let w = rot_window(q0, q1, ease, spin, 1.5);
                prop_assert!(crate::rotation_angle(w.value(0.0), q0) < 1e-12);
                prop_assert!(crate::rotation_angle(w.value(1.5), q1) < 1e-9);
            }
        }

        #[test]
        fn monotone_keys_give_a_monotone_curve(
            steps in prop::collection::vec((0.05f64..2.0, 0.0f64..10.0), 2..10),
        ) {
            let mut keys = vec![knot(0.0, 0.0, Ease::SMOOTH)];
            for (dt, dv) in steps {
                let last = keys[keys.len() - 1];
                keys.push(knot(last.t + dt, last.v + dv, Ease::SMOOTH));
            }
            let end = keys[keys.len() - 1].t;
            let mut prev = f64::NEG_INFINITY;
            for i in 0..=2000 {
                let v = eval(&keys, end * f64::from(i) / 2000.0);
                prop_assert!(v >= prev - 1e-9, "{v} after {prev}");
                prev = v;
            }
        }
    }
}
