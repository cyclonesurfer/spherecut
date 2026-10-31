# c1check2.py prints "clamped 2.0e+00" for the clamped spline. That is the
# measure, not the spline: where the clamp stops the camera at a key, both
# one-sided velocities are about 0 and their difference is finite-difference
# noise, so the jump relative to the speed at the key is about 2. Measured
# against the larger chord speed either side, the clamped spline is C1.
import os
src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'c1check2.py')).read()
exec(src.split("for r in [1.0001, 3, 15]:")[0])

def jumps(ratio_hi, trials=300, n=6):
    rel_key = rel_chord = 0.0
    for _ in range(trials):
        qs = [rand_rot()]
        for _ in range(n-1): qs.append(near_rot(qs[-1], 150))
        qs = canonicalise(qs)
        ts = np.cumsum([0] + list(rng.uniform(1, ratio_hi, n-1)))
        ev = make(qs, ts, clamp=True)
        for i in range(1, n-1):
            wl = omega(ev, ts[i], i-1, side=-1); wr = omega(ev, ts[i], i, side=+1)
            j = np.linalg.norm(wl - wr)
            rel_key = max(rel_key, j / max(np.linalg.norm(wl), np.linalg.norm(wr), 1e-9))
            c = max(angle(qs[i-1], qs[i])/(ts[i]-ts[i-1]), angle(qs[i], qs[i+1])/(ts[i+1]-ts[i]))
            rel_chord = max(rel_chord, j / c)
    return rel_key, rel_chord

for r in [1.0001, 3, 15]:
    a, b = jumps(r)
    print(f"clamped, interval ratio up to {r:g}: jump / speed at the key {a:.1e}; jump / larger chord speed {b:.1e}")
