#### G-A

UNMAPs (decode-v0's AMDGPU_GEM_VA immediately followed by GEM_CLOSE): 22. Main-thread epoll_pwait of 40 ms or more: 57. UNMAP to the start of the next such wait: n 10, p50 137.3 ms, p5 16.1, p95 618.9; within 0–20 ms: 1 (10 %).

UNMAPs between the first and second decode-v0 SYNCOBJ wait of 10 ms or more after a GEM_CREATE: 2 of 22.

GEM_CREATE, GEM_VA and GEM_CLOSE per thread, counts in run seconds 0 to 20:

- decode-v0 789074 AMDGPU_GEM_CREATE: 153: 40 7 5 5 5 5 5 7 6 6 5 6 5 6 7 7 5 7 6 7 1
- decode-v0 789074 AMDGPU_GEM_VA: 298: 57 17 11 10 10 10 10 13 11 12 11 12 10 12 13 14 11 14 12 12 16
- decode-v0 789074 GEM_CLOSE: 147: 19 10 6 5 5 5 5 6 5 6 6 6 5 6 6 7 6 7 6 6 14
- doctor 789069 AMDGPU_GEM_CREATE: 218: 218 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789069 AMDGPU_GEM_VA: 430: 430 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- doctor 789069 GEM_CLOSE: 209: 209 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789063 AMDGPU_GEM_CREATE: 195: 195 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut 789063 AMDGPU_GEM_VA: 728: 200 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 528
- spherecut 789063 GEM_CLOSE: 535: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 530
- spherecut:cs0 789075 AMDGPU_GEM_VA: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:cs0 789075 GEM_CLOSE: 5: 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789077 AMDGPU_GEM_CREATE: 4: 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789077 AMDGPU_GEM_VA: 3: 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- spherecut:sh0 789080 AMDGPU_GEM_CREATE: 1: 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789068 AMDGPU_GEM_CREATE: 322: 248 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
- upload 789068 AMDGPU_GEM_VA: 323: 248 37 37 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1
- upload 789068 GEM_CLOSE: 1: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1

