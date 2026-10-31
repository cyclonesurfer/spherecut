# Why the De Casteljau/slerp form of the non-uniform Catmull-Rom spline fails
# C1 at uneven spacing (c1check.py's "cr non-uniform"): a control rotation
# (the key velocity times a third of the interval) passes 180 degrees, and
# slerp then takes the other branch. Jumps are measured against the larger
# chord speed either side of the key.
import os
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'c1check.py')).read().split("for kind in ['squad'")[0])

def worst(max_deg, trials=300, n=6):
    w_all = w_small = 0.0; big = 0
    for _ in range(trials):
        qs = [rand_rot()]
        for _ in range(n - 1): qs.append(near_rot(qs[-1], max_deg))
        qs = canonicalise(qs)
        ts = np.cumsum([0] + list(rng.uniform(0.2, 3.0, n - 1)))
        ctl = 0.0   # largest control rotation, full angle
        for i in range(1, n - 1):
            d0, d1 = ts[i] - ts[i-1], ts[i+1] - ts[i]
            r0 = 2*qlog(mul(qs[i], inv(qs[i-1]))) / d0; r1 = 2*qlog(mul(qs[i+1], inv(qs[i]))) / d1
            w = (d1*r0 + d0*r1) / (d0 + d1)
            ctl = max(ctl, np.linalg.norm(w) * max(d0, d1) / 3)
        ev = make_eval('cr', qs, ts)
        j = 0.0
        for i in range(1, n - 1):
            wl = omega(ev, ts[i], i-1, side=-1); wr = omega(ev, ts[i], i, side=+1)
            c = max(angle(qs[i-1], qs[i])/(ts[i]-ts[i-1]), angle(qs[i], qs[i+1])/(ts[i+1]-ts[i]))
            j = max(j, np.linalg.norm(wl - wr) / c)
        w_all = max(w_all, j)
        if ctl < np.pi: w_small = max(w_small, j)
        else: big += 1
    return w_all, w_small, big

for md in (30, 120):
    a, s, b = worst(md)
    print(f"keys up to {md} deg apart, spacing 0.2-3 s: worst jump {a:.1e}; "
          f"tracks whose controls stay under 180 deg {s:.1e}; tracks with a control past 180 deg: {b}/300")
