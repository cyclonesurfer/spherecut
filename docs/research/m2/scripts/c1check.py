# Numerical C1 / overshoot checks for quaternion keyframe splines.
# Quaternions are numpy arrays [w, x, y, z]. log/exp use the half-angle
# convention: q = exp(v) with |v| = theta/2.
import numpy as np
rng = np.random.default_rng(7)

def mul(a, b):
    w1, x1, y1, z1 = a; w2, x2, y2, z2 = b
    return np.array([w1*w2 - x1*x2 - y1*y2 - z1*z2,
                     w1*x2 + x1*w2 + y1*z2 - z1*y2,
                     w1*y2 - x1*z2 + y1*w2 + z1*x2,
                     w1*z2 + x1*y2 - y1*x2 + z1*w2])
def inv(q): return q * np.array([1, -1, -1, -1])
def qexp(v):
    a = np.linalg.norm(v)
    if a < 1e-15: return np.array([1.0, *v])
    return np.concatenate([[np.cos(a)], np.sin(a) * v / a])
def qlog(q):
    q = q / np.linalg.norm(q)
    s = np.linalg.norm(q[1:])
    if s < 1e-15: return np.zeros(3)
    return np.arctan2(s, q[0]) * q[1:] / s
def qpow(q, t): return qexp(t * qlog(q))
def slerp(a, b, t): return mul(qpow(mul(b, inv(a)), t), a)  # no sign fix: keys pre-canonicalised
def angle(a, b):
    r = mul(inv(a), b); return 2 * np.arctan2(np.linalg.norm(r[1:]), abs(r[0]))
def rand_rot():
    q = rng.normal(size=4); return q / np.linalg.norm(q)
def near_rot(q, max_deg):
    ax = rng.normal(size=3); ax /= np.linalg.norm(ax)
    th = np.radians(rng.uniform(10, max_deg))
    return mul(qexp(ax * th / 2), q)
def canonicalise(keys):
    out = [keys[0]]
    for q in keys[1:]:
        out.append(-q if np.dot(out[-1], q) < 0 else q)
    return out

# ---- Squad with Shoemake's (uniform) inner points, body-frame form.
def squad_inner(qs):
    s = []
    for i in range(len(qs)):
        if i == 0 or i == len(qs) - 1: s.append(qs[i]); continue
        qi = qs[i]
        v = (qlog(mul(inv(qi), qs[i+1])) + qlog(mul(inv(qi), qs[i-1]))) / 4
        s.append(mul(qi, qexp(-v)))
    return s
def squad_seg(p, a, b, q, u):
    return slerp(slerp(p, q, u), slerp(a, b, u), 2 * u * (1 - u))

# ---- Non-uniform Catmull-Rom-like Bezier controls (Geier, splines docs).
def cr_controls(qs, ts):
    n = len(qs); plus = [None]*n; minus = [None]*n
    for i in range(n):
        if i == 0 or i == n - 1:
            plus[i] = minus[i] = qs[i]; continue  # zero end tangents (simple)
        d0, d1 = ts[i] - ts[i-1], ts[i+1] - ts[i]
        r0 = qlog(mul(qs[i], inv(qs[i-1]))) / d0
        r1 = qlog(mul(qs[i+1], inv(qs[i]))) / d1
        w = (d1 * r0 + d0 * r1) / (d0 + d1)
        plus[i] = mul(qexp(d1 / 3 * w), qs[i])
        minus[i] = mul(inv(qexp(d0 / 3 * w)), qs[i])
    return plus, minus
def decasteljau(c0, c1, c2, c3, u):
    a, b, c = slerp(c0, c1, u), slerp(c1, c2, u), slerp(c2, c3, u)
    return slerp(slerp(a, b, u), slerp(b, c, u), u)
# ---- Kim-Kim-Shin cumulative Bezier form on the same four controls.
def kks_bezier(c0, c1, c2, c3, u):
    w1 = qlog(mul(inv(c0), c1)); w2 = qlog(mul(inv(c1), c2)); w3 = qlog(mul(inv(c2), c3))
    b1 = 1 - (1 - u)**3; b2 = 3*u*u - 2*u**3; b3 = u**3
    return mul(mul(mul(c0, qexp(w1*b1)), qexp(w2*b2)), qexp(w3*b3))

def make_eval(kind, qs, ts, uniform_param=False):
    if kind == 'squad': s = squad_inner(qs)
    else: plus, minus = cr_controls(qs, ts)
    def ev(t, seg):
        u = (t - ts[seg]) / (ts[seg+1] - ts[seg])
        if kind == 'squad': return squad_seg(qs[seg], s[seg], s[seg+1], qs[seg+1], u)
        c = (qs[seg], plus[seg], minus[seg+1], qs[seg+1])
        return decasteljau(*c, u) if kind == 'cr' else kks_bezier(*c, u)
    return ev

def omega(ev, t, seg, h=1e-6, side=+1):
    # world angular velocity 2*vec(q' q^-1), one-sided difference inside `seg`.
    a = ev(t, seg); b = ev(t + side*h, seg)
    if np.dot(a, b) < 0: b = -b
    dq = (b - a) / (side*h)
    return 2 * mul(dq, inv(a))[1:]

def jumps(kind, n=6, uniform=False, max_deg=120, trials=200):
    worst = 0.0
    for _ in range(trials):
        qs = [rand_rot()]
        for _ in range(n - 1): qs.append(near_rot(qs[-1], max_deg))
        qs = canonicalise(qs)
        ts = np.cumsum([0] + list(np.ones(n-1) if uniform else rng.uniform(0.2, 3.0, n-1)))
        ev = make_eval(kind, qs, ts)
        for i in range(1, n - 1):
            wl = omega(ev, ts[i], i-1, side=-1)
            wr = omega(ev, ts[i], i, side=+1)
            scale = max(np.linalg.norm(wl), np.linalg.norm(wr), 1e-9)
            worst = max(worst, np.linalg.norm(wl - wr) / scale)
    return worst

for kind in ['squad', 'cr', 'kks']:
    for uniform in [True, False]:
        print(f"{kind:5s} {'uniform' if uniform else 'non-uniform':11s} worst relative angular-velocity jump at interior keys: {jumps(kind, uniform=uniform):.2e}")

# Do De Casteljau-slerp and KKS cumulative Bezier give the same curve on the same controls?
diff = 0
for _ in range(200):
    c = canonicalise([rand_rot()] + [None]*0) 
    c0 = rand_rot(); c1 = near_rot(c0, 60); c2 = near_rot(c1, 60); c3 = near_rot(c2, 60)
    c0, c1, c2, c3 = canonicalise([c0, c1, c2, c3])
    for u in np.linspace(0, 1, 11):
        diff = max(diff, angle(decasteljau(c0, c1, c2, c3, u), kks_bezier(c0, c1, c2, c3, u)))
print(f"max angle between De Casteljau-slerp and KKS cumulative Bezier on same controls: {np.degrees(diff):.3f} deg")

# Overshoot: yaw keys 0, 90, 90, 0 at t = 0,1,2,3 with Catmull-Rom tangents.
def yaw_q(deg): return qexp(np.array([0, -np.radians(deg) / 2, 0]))  # R_y(-yaw) per D33
qs = canonicalise([yaw_q(0), yaw_q(90), yaw_q(90), yaw_q(0)]); ts = np.array([0., 1, 2, 3])
ev = make_eval('cr', qs, ts)
peak = max(angle(ev(t, 1), yaw_q(0)) for t in np.linspace(1, 2, 201))
print(f"CR overshoot: keys yaw 0,90,90,0 -> max yaw between the two 90 keys = {np.degrees(peak):.2f} deg")
