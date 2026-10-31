# Cumulative (KKS-style) cubic Hermite on SO(3), fed rotation vectors directly,
# with non-uniform Catmull-Rom angular velocities, optional clamp, per-key ease.
import os
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'c1check.py')).read().split("for kind in ['squad'")[0])

def rotvec_rel(a, b):  # body-frame rotation vector (full angle) of a^-1 b, shortest
    r = mul(inv(a), b); r = -r if r[0] < 0 else r
    return 2 * qlog(r)
def expv(v): return qexp(v / 2)       # full-angle rotation vector -> quaternion
def logv(q): return 2 * qlog(q if q[0] >= 0 else -q)

def tangents(qs, ts, clamp=False, ease=None):
    """World-frame angular velocity at each key (rad/s)."""
    n = len(qs); w = [np.zeros(3) for _ in range(n)]
    rho = [logv(mul(qs[i+1], inv(qs[i]))) / (ts[i+1]-ts[i]) for i in range(n-1)]
    for i in range(1, n-1):
        d0, d1 = ts[i]-ts[i-1], ts[i+1]-ts[i]
        wi = (d1*rho[i-1] + d0*rho[i]) / (d0+d1)
        if clamp:
            # reversal -> stop; else limit speed to 3x the slower neighbouring chord speed
            if np.dot(rho[i-1], rho[i]) <= 0: wi = np.zeros(3)
            else:
                lim = 3 * min(np.linalg.norm(rho[i-1]), np.linalg.norm(rho[i]))
                s = np.linalg.norm(wi); wi = wi if s <= lim else wi * lim / s
        w[i] = wi
    if ease is not None:
        for i, e in enumerate(ease): w[i] = w[i] * e   # e = 0 eases to a stop at key i
    return w

def hermite_seg(q0, q1, w0, w1, d, u):
    """Cumulative cubic Bezier/Hermite: q(u) = q0 exp(b1 a) exp(b2 m) exp(b3 c).
    a, c are body-frame control rotations from the end angular velocities;
    m closes the gap so that q(1) = q1 exactly."""
    a = (d/3) * rotate_inv(q0, w0)          # body frame at q0
    c = (d/3) * rotate_inv(q1, w1)          # body frame at q1
    m = logv(mul(mul(inv(expv(a)), mul(inv(q0), q1)), inv(expv(c))))
    b1 = 1-(1-u)**3; b2 = 3*u*u-2*u**3; b3 = u**3
    return mul(mul(mul(q0, expv(b1*a)), expv(b2*m)), expv(b3*c))
def rotate_inv(q, v):  # q^-1 v q (world vector -> body frame)
    return mul(mul(inv(q), np.array([0, *v])), q)[1:]

def make(qs, ts, **kw):
    w = tangents(qs, ts, **kw)
    return lambda t, s: hermite_seg(qs[s], qs[s+1], w[s], w[s+1], ts[s+1]-ts[s], (t-ts[s])/(ts[s+1]-ts[s]))

def worst_jump(ratio_hi, trials=300, n=6, **kw):
    worst = 0
    for _ in range(trials):
        qs = [rand_rot()]
        for _ in range(n-1): qs.append(near_rot(qs[-1], 150))
        qs = canonicalise(qs)
        ts = np.cumsum([0]+list(rng.uniform(1, ratio_hi, n-1)))
        ev = make(qs, ts, **kw)
        for i in range(1, n-1):
            wl = omega(ev, ts[i], i-1, side=-1); wr = omega(ev, ts[i], i, side=+1)
            worst = max(worst, np.linalg.norm(wl-wr)/max(np.linalg.norm(wl), np.linalg.norm(wr), 1e-9))
            assert angle(ev(ts[i], i-1), qs[i]) < 1e-9 and angle(ev(ts[i], i), qs[i]) < 1e-9
    return worst
for r in [1.0001, 3, 15]:
    print(f"cumulative Hermite, interval ratio up to {r:g}: worst rel. jump {worst_jump(r):.1e}; clamped {worst_jump(r, clamp=True):.1e}")

def yaw_q(deg): return expv(np.array([0, -np.radians(deg), 0]))
for clamp in [False, True]:
    qs = canonicalise([yaw_q(0), yaw_q(90), yaw_q(90), yaw_q(0)]); ts = np.array([0., 1, 2, 3])
    ev = make(qs, ts, clamp=clamp)
    peak = max(angle(ev(t, 1), yaw_q(0)) for t in np.linspace(1, 2, 401))
    print(f"yaw keys 0,90,90,0: peak between the 90s = {np.degrees(peak):.2f} deg (clamp={clamp})")
# uneven spacing overshoot: yaw 0 @0, 10 @0.1, 100 @3
for clamp in [False, True]:
    qs = canonicalise([yaw_q(0), yaw_q(10), yaw_q(100)]); ts = np.array([0., 0.1, 3.0])
    ev = make(qs, ts, clamp=clamp)
    ys = [np.degrees(angle(ev(t, 1), yaw_q(0))) for t in np.linspace(0.1, 3.0, 2001)]
    # largest step back (non-monotonic yaw) and max overshoot beyond 100
    mono = min(np.diff(ys)); print(f"yaw 0@0,10@0.1,100@3: min dyaw/step {mono:.3f}, max {max(ys):.2f} deg (clamp={clamp})")
# Winding: a 270-degree yaw spin in one segment, via m on the long branch.
q0, q1 = yaw_q(0), yaw_q(270)
a = np.zeros(3); c = np.zeros(3)
m_short = logv(mul(inv(q0), q1)); ax = m_short/np.linalg.norm(m_short)
m_long = m_short - 2*np.pi*ax   # same end rotation, the other way round (-90 -> +270)
path = [mul(q0, expv((3*u*u-2*u**3)*m_long)) for u in np.linspace(0,1,9)]
print("270 spin via long branch, forward yaw (deg):", [round(float(np.degrees(np.arctan2((mul(mul(p, np.array([0,0,0,-1])), inv(p))[1:])[0], -(mul(mul(p, np.array([0,0,0,-1])), inv(p))[1:])[2]))),1) for p in path])
