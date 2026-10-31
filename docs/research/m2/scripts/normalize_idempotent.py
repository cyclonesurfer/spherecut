# Is glam-style normalisation (q * (1/sqrt(dot(q,q)))) idempotent in f64?
# Section 3.1 normalises quaternions on input; section 6.6 wants evaluation
# bit-identical after save and load.
import numpy as np
rng=np.random.default_rng(11)
def norm(q):
    d=q[0]*q[0]+q[1]*q[1]+q[2]*q[2]+q[3]*q[3]   # glam's dot, in order
    return q*(1.0/np.sqrt(d))
n=200000; changed=0
for _ in range(n):
    q=norm(rng.normal(size=4))
    if not np.array_equal(norm(q),q): changed+=1
print(f"{changed} of {n} normalised quaternions change bits when normalised again ({100*changed/n:.1f} %)")
