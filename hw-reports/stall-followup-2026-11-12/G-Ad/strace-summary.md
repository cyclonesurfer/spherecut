#### G-Ad

UNMAPs (decode-v0's AMDGPU_GEM_VA immediately followed by GEM_CLOSE): 24. Main-thread epoll_pwait of 40 ms or more: 0. UNMAP to the start of the next such wait: n 0, p50 – ms, p5 –, p95 –; within 0–20 ms: 0 (0 %).

UNMAPs between the first and second decode-v0 SYNCOBJ wait of 10 ms or more after a GEM_CREATE: 0 of 24.

GEM_CREATE, GEM_VA and GEM_CLOSE per thread, counts in run seconds 0 to 20:

- decode-v0 789265 AMDGPU_GEM_CREATE: 134: 34 8 4 4 4 4 4 7 7 6 8 5 4 4 4 6 7 4 5 4 1
- decode-v0 789265 AMDGPU_GEM_VA: 264: 50 13 12 9 9 8 7 13 15 10 18 10 8 8 7 13 14 8 10 7 15
- decode-v0 789265 GEM_CLOSE: 132: 17 6 8 5 5 4 3 6 8 4 10 5 4 4 3 7 7 4 5 3 14
- doctor 789260 AMDGPU_GEM_CREATE: 221: 221 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789260 AMDGPU_GEM_VA: 433: 433 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789260 GEM_CLOSE: 210: 210 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789254 AMDGPU_GEM_CREATE: 202: 198 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789254 AMDGPU_GEM_VA: 670: 203 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 463
- spherecut 789254 GEM_CLOSE: 470: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 465
- spherecut:cs0 789266 AMDGPU_GEM_VA: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:cs0 789266 GEM_CLOSE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789268 AMDGPU_GEM_CREATE: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789268 AMDGPU_GEM_VA: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789271 AMDGPU_GEM_CREATE: 2: 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789271 AMDGPU_GEM_VA: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789259 AMDGPU_GEM_CREATE: 315: 204 0 37 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789259 AMDGPU_GEM_VA: 380: 204 0 37 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 65
- upload 789259 GEM_CLOSE: 65: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 65

