# Section 6.6's C1 test: one-sided finite differences at h = 1e-6 s must agree
# within 1e-5 of the segment's chord speed. Scalar spline of section 6.2
# (non-uniform Catmull-Rom slopes, Fritsch-Carlson clamp), smooth keys.
import numpy as np
rng=np.random.default_rng(3)
def slopes(t,v):
    n=len(t); m=np.zeros(n); r=np.diff(v)/np.diff(t)
    for i in range(1,n-1):
        d0,d1=t[i]-t[i-1],t[i+1]-t[i]
        w=(d1*r[i-1]+d0*r[i])/(d0+d1)
        if r[i-1]*r[i]<=0: w=0.0
        else: w=np.sign(w)*min(abs(w),3*min(abs(r[i-1]),abs(r[i])))
        m[i]=w
    return m,r
def ev(t,v,m,x,s):
    d=t[s+1]-t[s]; u=(x-t[s])/d
    h00=2*u**3-3*u**2+1; h10=u**3-2*u**2+u; h01=-2*u**3+3*u**2; h11=u**3-u**2
    return h00*v[s]+h10*d*m[s]+h01*v[s+1]+h11*d*m[s+1]
for base in [1/30, 0.1, 0.5, 1.0, 5.0]:
    worst=0; fails=0; tot=0
    for _ in range(2000):
        n=rng.integers(3,13)
        dt=base*rng.uniform(1,20,n-1)        # ratios up to 20:1, shortest about `base`
        t=np.concatenate([[0],np.cumsum(dt)]); v=np.radians(rng.uniform(-180,180,n))
        m,r=slopes(t,v); h=1e-6
        for i in range(1,n-1):
            if m[i]==0: continue
            left=(ev(t,v,m,t[i],i-1)-ev(t,v,m,t[i]-h,i-1))/h
            right=(ev(t,v,m,t[i]+h,i)-ev(t,v,m,t[i],i))/h
            rel=abs(left-right)/max(abs(r[i]),abs(r[i-1]))
            worst=max(worst,rel); tot+=1; fails+= rel>1e-5
    print(f"shortest spacing ~{base:.3f} s: worst {worst:.1e}, {fails}/{tot} keys over 1e-5")
