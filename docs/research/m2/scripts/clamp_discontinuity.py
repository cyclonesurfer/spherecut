# Free-mode clamp of the plan (section 6.2): omega_i = 0 when chords "turn back"
# (dot <= 0), else |omega_i| <= 3 min(|rho|). Show it is discontinuous in the keys:
# a pan right 60 deg, then a tilt up 30 deg with a tiny extra yaw +-eps.
# Then the same with the limiter M2.2 uses instead (amendment 2, finding 2):
# omega scaled by max(0, cos) of the angle between the chords, then the cap.
import numpy as np
def mul(a,b):
    w1,x1,y1,z1=a; w2,x2,y2,z2=b
    return np.array([w1*w2-x1*x2-y1*y2-z1*z2, w1*x2+x1*w2+y1*z2-z1*y2,
                     w1*y2-x1*z2+y1*w2+z1*x2, w1*z2+x1*y2-y1*x2+z1*w2])
def inv(q): return q*np.array([1,-1,-1,-1])
def qexp(v):
    a=np.linalg.norm(v)
    return np.array([1.0,*v]) if a<1e-15 else np.concatenate([[np.cos(a)],np.sin(a)*v/a])
def qlog(q):
    q=q/np.linalg.norm(q); s=np.linalg.norm(q[1:])
    return np.zeros(3) if s<1e-15 else np.arctan2(s,q[0])*q[1:]/s
def expv(v): return qexp(v/2)
def logv(q): return 2*qlog(q if q[0]>=0 else -q)
def rot_inv(q,v): return mul(mul(inv(q),np.array([0,*v])),q)[1:]
def angle(a,b):
    r=mul(inv(a),b); return 2*np.arctan2(np.linalg.norm(r[1:]),abs(r[0]))
def Ry(a): return expv(np.array([0,a,0]))
def Rx(a): return expv(np.array([a,0,0]))
def Rz(a): return expv(np.array([0,0,a]))
def ypr(y,p,r): return mul(mul(Ry(-np.radians(y)),Rx(np.radians(p))),Rz(-np.radians(r)))
def tangents(qs,ts,limiter):
    n=len(qs); w=[np.zeros(3) for _ in range(n)]
    rho=[logv(mul(qs[i+1],inv(qs[i])))/(ts[i+1]-ts[i]) for i in range(n-1)]
    for i in range(1,n-1):
        d0,d1=ts[i]-ts[i-1],ts[i+1]-ts[i]
        wi=(d1*rho[i-1]+d0*rho[i])/(d0+d1)
        lengths=np.linalg.norm(rho[i-1])*np.linalg.norm(rho[i])
        cos=np.dot(rho[i-1],rho[i])/lengths if lengths>0 else 0.0
        if limiter=='cos': wi=wi*max(cos,0.0)
        if cos<=0: wi=np.zeros(3)
        else:
            lim=3*min(np.linalg.norm(rho[i-1]),np.linalg.norm(rho[i])); s=np.linalg.norm(wi)
            if s>lim: wi=wi*lim/s
        w[i]=wi
    return w, rho
def seg(q0,q1,w0,w1,d,u):
    a=(d/3)*rot_inv(q0,w0); c=(d/3)*rot_inv(q1,w1)
    m=logv(mul(mul(inv(expv(a)),mul(inv(q0),q1)),inv(expv(c))))
    b1=1-(1-u)**3; b2=3*u*u-2*u**3; b3=u**3
    return mul(mul(mul(q0,expv(b1*a)),expv(b2*m)),expv(b3*c))
def path(eps,limiter):
    # a pan right then a tilt up, with a small pan of eps degrees on the tilt
    qs=[ypr(0,0,0), ypr(60,0,0), ypr(60+eps,30,0), ypr(60+eps,30,0)]
    ts=np.array([0.,2.,3.,4.])
    w,rho=tangents(qs,ts,limiter)
    out=[]
    for s in range(3):
        for u in np.linspace(0,1,201):
            out.append(seg(qs[s],qs[s+1],w[s],w[s+1],ts[s+1]-ts[s],u))
    return out, w, rho
for limiter in ['step','cos']:
    print(f"limiter {limiter}:")
    for eps in [0.0, -0.01, 0.01]:
        _, w, rho = path(eps, limiter)
        print(f"  eps {eps:+.2f} deg: dot(rho0,rho1) = {np.dot(rho[0],rho[1]):+.3e}, |omega_1| = {np.degrees(np.linalg.norm(w[1])):.3f} deg/s")
    a,_,_=path(-0.01, limiter); b,_,_=path(0.01, limiter)
    print("  largest sample difference between eps=-0.01 and +0.01 deg: %.4f deg" % np.degrees(max(angle(x,y) for x,y in zip(a,b))))
