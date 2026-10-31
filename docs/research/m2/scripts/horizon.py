# Check section 6's horizon-tilt numbers: roll of the slerp midpoint between
# two level keys, in D33's convention q = Ry(-yaw) Rx(pitch) Rz(-roll).
import numpy as np
def mul(a,b):
    w1,x1,y1,z1=a; w2,x2,y2,z2=b
    return np.array([w1*w2-x1*x2-y1*y2-z1*z2, w1*x2+x1*w2+y1*z2-z1*y2,
                     w1*y2-x1*z2+y1*w2+z1*x2, w1*z2+x1*y2-y1*x2+z1*w2])
def inv(q): return q*np.array([1,-1,-1,-1])
def axis(v,a): v=np.array(v,float); return np.concatenate([[np.cos(a/2)],np.sin(a/2)*v])
def rot(q,v): return mul(mul(q,np.array([0,*v])),inv(q))[1:]
def ypr_q(y,p,r): return mul(mul(axis([0,1,0],-np.radians(y)),axis([1,0,0],np.radians(p))),axis([0,0,1],-np.radians(r)))
def from_q(q):
    f=rot(q,[0,0,-1]); level=np.hypot(f[0],f[2]); pitch=np.arctan2(f[1],level)
    yaw=np.arctan2(f[0],-f[2])
    rest=mul(inv(mul(axis([0,1,0],-yaw),axis([1,0,0],pitch))),q)
    roll=-2*np.arctan2(rest[3],rest[0])
    return np.degrees([yaw,pitch,(roll+np.pi)%(2*np.pi)-np.pi])
def slerp(a,b,t):
    if np.dot(a,b)<0: b=-b
    d=np.clip(np.dot(a,b),-1,1); th=np.arccos(d)
    return a if th<1e-12 else (np.sin((1-t)*th)*a+np.sin(t*th)*b)/np.sin(th)
for (a,b) in [((0,0),(90,30)), ((0,-30),(120,30)), ((0,0),(170,60))]:
    qa=ypr_q(a[0],a[1],0); qb=ypr_q(b[0],b[1],0)
    mid=slerp(qa,qb,0.5)
    worst=max(abs(from_q(slerp(qa,qb,t))[2]) for t in np.linspace(0,1,1001))
    print(a,b,"midpoint ypr",np.round(from_q(mid),2),"max |roll| on the path %.2f"%worst)
