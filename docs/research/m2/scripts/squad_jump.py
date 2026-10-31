# Squad's angular-velocity jump at keys with uneven spacing, relative to the
# chord speeds either side (the research scripts' squad, same random setup).
import os
import numpy as np
src=open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'c1check.py')).read().split("for kind in ['squad'")[0]
exec(src)
worst_min=worst_max=worst_speed=0
for _ in range(300):
    n=6; qs=[rand_rot()]
    for _ in range(n-1): qs.append(near_rot(qs[-1],120))
    qs=canonicalise(qs); ts=np.cumsum([0]+list(rng.uniform(0.2,3.0,n-1)))
    ev=make_eval('squad',qs,ts)
    for i in range(1,n-1):
        wl=omega(ev,ts[i],i-1,side=-1); wr=omega(ev,ts[i],i,side=+1)
        c0=angle(qs[i-1],qs[i])/(ts[i]-ts[i-1]); c1=angle(qs[i],qs[i+1])/(ts[i+1]-ts[i])
        j=np.linalg.norm(wl-wr)
        worst_min=max(worst_min,j/min(c0,c1)); worst_max=max(worst_max,j/max(c0,c1))
        sl,sr=np.linalg.norm(wl),np.linalg.norm(wr)
        worst_speed=max(worst_speed,max(sl,sr)/min(sl,sr))
print("squad, spacing 0.2..3 s: jump/min chord speed %.2f, jump/max chord speed %.2f, speed ratio across key %.2f"%(worst_min,worst_max,worst_speed))
