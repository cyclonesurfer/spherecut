# DJI Osmo 360 (`.OSV`): format notes

Researched 2026-10-22. **No sample files were examined for this note.** Every
statement comes from reading third-party source code, format notes written by
other projects, or spec sheets, and each one is cited by number (sources are
listed at the end). Spherecut has not yet checked any of it against a real
file. The confidence tags mean:

- **[C]** corroborated: two or more independent implementations or documents agree.
- **[S]** single source: one project or document says it, nothing contradicts it.
- **[U]** unverified, inferred, or **conflicting** (see §9). Treat it as a hypothesis.

"Raw" is a misnomer here. An `.OSV` is not sensor RAW: it holds two
compressed, unstitched HEVC 10-bit 4:2:0 fisheye streams plus metadata
[14][18][20]. Throughout, "raw" means "unstitched camera original".

## 0. Summary

| Question | Answer |
|---|---|
| Container | ISO-BMFF (`ftyp isom`), MP4/MOV family. FFmpeg demuxes it with no special handling [7][14][15][17][24]. **[C]** |
| Video | **Two separate `hvc1` tracks, one square fisheye per lens.** Not side by side in one frame. HEVC Main10, 4:2:0, 3840² (8K modes), 3000² (6K) or 1920² (4K100) per lens [7][14][16][17][20]. **[C]** |
| Audio | One AAC-LC track, 48 kHz, stereo, from four built-in mics. **No ambisonic/360 audio** is reported anywhere [7][14][17][26][32]. **[C]** |
| Metadata | Two `djmd` tracks ("CAM meta", protobuf, one sample per frame) and two `dbgi` tracks ("CAM dbgi", ISP debug). Also a top-level `camd` box (a nested mini-MP4) and a 688×344 equirect JPEG in `udta/meta/ilst/covr` [7][15][16][17][19]. **[C]** |
| Calibration | Per-unit, per-clip, in `djmd` sample 0: 24 `DewarpParams` slots (12 slave/master pairs). The model is an equidistant θ-polynomial (k1..k5) plus a tangential pair [2][7][9][13][16]. **[C]**, with model details **[U]** (§3.3) |
| Telemetry | Osmo 360: **fused attitude quaternions only** (~1 kHz, ~1/fps of a second per packet), plus one accelerometer vector per frame. **No raw gyro** [1][4][7][8][13]. **[C]** Osmo 360 II: raw gyro (rad/s) and accel (g) as well [1][4]. **[S]** |
| telemetry-parser | Supported on **git master only** (PR #55, 2026-06-09; rig, timing and calibration rework 2026-10-08). The crates.io release (0.2.6, 2022) predates it [6][31]. **[C]** |
| Proxy | `.LRF` beside each `.OSV`: one 2048×1024 H.264 track plus the same `djmd` metadata. **Its layout is disputed**: side-by-side fisheyes [7][10] versus stitched equirect [19]. **[U]** |
| DJI stitched export | Equirect 2:1 MP4 from DJI Studio or Mimo [30][32]. **Whether it carries spherical metadata (`sv3d`/GSpherical) is unknown.** **[U]** |
| **Verdict** | **Full raw support is feasible without any closed SDK** (§10). The blockers are validation questions, not missing information. |

## 1. Models and firmware

| Body | `product_name` | Schema (`proto_file_name`) | Notes |
|---|---|---|---|
| Osmo 360 (launched 2025-07-31 [25][27]) | `Osmo 360` (also matched: `DJI Osmo360`) [2] | `dvtm_oq101.proto`, library 02.01.15, product proto 2.0.8, firmware 10.00.25.29 seen [6][9][16] | 2× 1/1.1″ square sensors, f/1.9 [25][9] |
| Osmo 360 II (announced IFA 2026-09-06 [28]) | `Osmo 360 II` [2] | Same field numbering as the Eagle 4 `wa530` schema [2] | Writes raw IMU, master lens on track 0 [1][2]. **[S]**, single-source format details |
| "Osmo 360 Air" | `Osmo 360 Air` [2] | wa530 numbering [2] | Named only in telemetry-parser. **I found no evidence the product exists [U]** |
| DJI Avata 360 (drone) | `DJI Avata360` [2] | `dvtm_AVATA360.proto`, wa530 numbering [2][7][13] | Also writes `.OSV`. 4 kHz attitude, no accel, no audio on the clip seen [7][13][18] |

**Choose the schema by `product_name`, not by `proto_file_name`.** DJI
reuses one schema under different declared names. Feeding an Avata 360
packet through the Osmo 360 numbering silently loses the calibration,
because `pano_dewarp_params` sits at `StreamMeta.6` in one schema and
`StreamMeta.5` in the other [2]. OpenOSV adds a fallback: it tries every known
numbering and keeps the one under which a calibration decodes [7]. **[C]**

## 2. Container

### 2.1 Box layout (6K clip, per [7]; corroborated in part by [16][17])

```
ftyp  isom (isom/iso2/mp41)
free  8 bytes
free  4052 bytes: index table, 16-byte entries  tag | u64 offset | u32 size   (covr, snal, camd)
mdat  per frame: djmd, djmd, dbgi, dbgi, video1, video2; audio chunks later
moov
  trak 1  vide hvc1  slave lens   tkhd flags 0x3 (enabled)
  trak 2  vide hvc1  master lens  tkhd flags 0x2 (NOT enabled)
  trak 3  soun mp4a  AAC-LC 48 kHz stereo
  trak 4  meta djmd  "CAM meta": full per-frame protobuf (calibration in sample 0, IMU)
  trak 5  meta djmd  "CAM meta": stripped copy (no calibration, no IMU)
  trak 6  meta dbgi  "CAM dbgi": per-lens ISP debug
  trak 7  meta dbgi  "CAM dbgi"
  udta    ©uid, dbpm, dbcm, btec, fsid, meta/ilst (covr, snal, tnal JPEGs, ©too), Xtra
  meta    empty (mdta)
camd  nested MP4 (ftyp/free/mdat/moov), two djmd tracks of ~120 samples (first 2 s + post-roll)
```

- The index table in the second `free` box points at `covr`, `snal` and
  `camd`. For `covr`/`snal` the offset points at the `ilst` item and the
  JPEG starts at offset+24 [7]. A remuxer that moves data must rewrite
  these offsets [15]. **[C]** (OpenOSV and osv2mkv both handle it.)
- `camd` is about 128 KB and repeats `djmd` metadata. It adds no calibration
  [16]. **[C]** ([7][16])
- Spherecut's parser must tolerate the top-level `camd` box and boxes after
  `moov` [7][17]. **[C]**

### 2.2 Video tracks

| Property | Value | Source |
|---|---|---|
| Codec | HEVC Main10 `hvc1`, `yuv420p10le` / P010 | [14][16][18][20] **[C]** |
| Per-lens size | 3840×3840 (8K modes, i.e. 7680×3840 output), 3000×3000 (6K), 1920×1920 (4K100) | [7][14][16][17][26] **[C]** |
| Coding | Coded 3008 wide with a 4-px conformance window, 3 slices per picture, GOP 60, no B-frames | [7] **[S]** |
| Bitrate | ~84.7 Mb/s per stream at 8K 29.97 (camera max 170 Mb/s total) | [20][25][26] **[S]** |
| Frame rates | 8K: 24/25/30/48/50; 6K: 24–60; 4K: 100 (360 modes) | [26][32] **[C]** |
| Colour tagging | `colr` 1/1/1 (BT.709) **even for D-Log M**. The real profile is `StreamMeta.4 color_mode` (0 normal, 9 HLG, 19 D-Log M) | [7][9] **[S]** |
| Lens to track | Track 1 (stream 0) = **slave**, optical axis −Y body. Track 2 (stream 1) = **master**, +Y body, the "front" of DJI's default view. **Reversed** on Osmo 360 II and Avata 360 (master = track 0) | [2][7][9][13] **[C]** |
| Timing | Both lens tracks share one `stts` and one `stss`. Dropped frames appear as longer `stts` entries (VFR). Per-frame capture timestamps live in `djmd`. After a gap, track 2's IDR can lag track 1's by one sample | [7] **[S]** |

Other tools label the tracks "front"/"rear" by order (osv2mov: first track =
front [14]; osv2mkv `--face front|rear` [15]). That contradicts the
master/slave mapping above. **Map lenses from the metadata, never by track
order or by these labels.** Which physical face (screen side or not) is the
master lens is **[U]**.

### 2.3 Audio

- One AAC-LC track, 48 kHz, stereo [7][14][17]. **[C]** The spec sheet lists
  "48 kHz, 16-bit, AAC" with Mono/Stereo recording modes and four built-in
  mics [26][32]. **[C]**
- No source mentions ambisonic, 4-channel or 360 audio for the Osmo 360 or the
  Osmo 360 II [25][26][28][32]. **[C]** for its absence in the spec sheets.
  Whether any firmware mode writes more channels is **[U]**.
- The Avata 360 `.OSV` that was inspected had no audio track [18]. **[S]**
- Wireless DJI Mic audio goes into the same AAC track [25][28]. That the
  layout is unchanged is **[U]**.

### 2.4 Metadata tracks

- `djmd` is the DJI "dvtm" protobuf (proto3). Each sample is one
  `ProductMeta { 1 ClipMeta, 2 StreamMeta, 3 FrameMeta }`. Sample 0 carries
  all three; later samples carry only `FrameMeta` [7][9][4]. **[C]**
- `dbgi` is protobuf (`dbginfo_oq101.proto`), an undocumented per-frame
  register/ISP dump of about 2 Mb/s and ~9 kB per sample [13][16]. No
  project decodes it. Ignore it. **[C]**
- Repeated scalars may be packed or unpacked. Parse leniently and skip unknown
  fields by wire type [7][9]. **[S]**

## 3. Lens layout, projection and calibration

### 3.1 Layout

Two back-to-back fisheyes on body ±Y [8]. Each lens is used to about 195°
(OpenOSV uses 195.18°, a figure read from DJI's stitcher [8][12]).
telemetry-parser computes per-lens validity half-angles of 98.56° and 99.12°
from the calibration on its Osmo 360 clip [2]. **[C]**, roughly 195–198°.
OpenOSV implies a baseline of about 2.5 cm ("Parallax for a 2.5 cm
baseline…") [8]. **[U]**

### 3.2 Calibration block (`StreamMeta.6 PanoDewarpParams`, Osmo 360 numbering)

There are 24 `DewarpParams` slots. Odd = slave, even = master: 1/2
native_refine, 3/4 native_refine_far, 5/6 lens_guards, 7/8 above-water,
9/10 under-water, 11/12 native (factory), and 13–24 a "far" ladder labelled
07, 09, 11, 12.5, 14, 16 [2][4][7][9]. **[C]** Unused slots are zero-filled.
On the OpenOSV sample only 1/2, 11/12 and 13–24 held data [7]. **[S]**

- The ladder labels are read as stitch distances (0.7 m … 1.6 m) [9]. **[U]**
  telemetry-parser instead reads them as a focal ladder "linear in the label"
  [2]. **[U]**
- **Default slot:** DJI's own tools start at `far_11` (17/18), then fall back to
  3/4, then 1/2 [2][7]. **[C]** (telemetry-parser hooked DJI Studio at runtime;
  OpenOSV studied DJI's software.) OpenOSV keeps `native_refine` and calls the
  difference noise (2.6 px focal) [7].
- `StreamMeta.7 extri_lens_mode`: 0 bare lenses, 1 lens guards, 2 underwater
  [4][7]. **[C]** DJI applies lens-protector mode as a **field-angle correction
  curve**, not as a calibration slot (+0.52° at 30°, +1.28° at 90°) [7].
  **[S]**, and the curve itself is DJI-derived (§11.2).

Fields of each `DewarpParams` [4][9][13][16] **[C]**:

| Field | Meaning |
|---|---|
| 1–4 | fx, fy, cx, cy in **calibration-frame px (3840×3840)** |
| 5–8, 15 | k1..k4, k5 (radial). 16–19 (k6..k9) are zero on every clip seen [11] |
| 9 | xi (unified model). 0 here [2][9] |
| 10, 11 | width, height (3840.0, as float) |
| 12–14 | yaw, pitch, roll in degrees (lens pose) |
| 20 | `p`, tangential pair (packed). 27 `tangent_coeff` duplicates it [2][9] |
| 21 | `q` (4 floats). OpenOSV says a copy of `cam_extri_q` on Osmo 360 [9]; four zeros on Avata 360 [7] |
| 22, 23 | `occlusion_pt_x/y`: body silhouette, 14 points, first point repeated [2][9]. PanoForge misread these as "radial LUTs" [16] |
| 24 | `lens_model` = 8.0 on Osmo 360 and Avata 360 [9][13] |
| 25, 30, 31, 35 | temperature (−1000 = unset), temperature-compensation enable, k, k_order [2][9] |
| 28 | `cam_extri_q` (w,x,y,z), equal to `Ry(yaw)·Rz(roll)·Rx(pitch)` to <0.03° [2][7] |

`ClipMeta.3 distortion_coefficients` (4 values) and `ClipMeta.8
digital_focal_length` describe the flat (single-lens) dewarp. Do not use them
for the 360 stitch [1][9]. **[C]**

**Per-unit:** yes. telemetry-parser calls the refined slots "the per-unit
refined calibration" [2]. The serial number is in
`ClipMetaHeader.product_sn` [4], and the two lenses' values differ [16].
**[C]** On the Avata 360, fx, cx and cy move by 1–3 px from clip to clip, so
read each clip's own header [13]. **[S]**

### 3.3 Lens model (some details unsettled)

```
θ_d = θ·(1 + k1θ² + k2θ⁴ + k3θ⁶ + k4θ⁸ + k5θ¹⁰)      (k5 is required; without it the curve folds before 90°)
x' = θ_d·X/ρ,  y' = θ_d·Y/ρ
xd = x' + 2p1·x'y' + p2(θ_d² + 2x'²),  yd = y' + p1(θ_d² + 2y'²) + 2p2·x'y'
u = fx·xd + cx,  v = fy·yd + cy
```

- telemetry-parser uses this model, and applies DJI's legacy fixup: when `p`
  is all zero, k3/k4 are reinterpreted as p1/p2 and zeroed [2].
- spirula-studio measured on Avata 360 frames that k5 is a fifth radial term
  and that `p` is tangential "in that order and sign". Lens-to-lens error drops
  from 2.0–2.2 px to ~0.9 px with it [13]. **[S]**, on Avata, not Osmo 360.
- **Conflict:** OpenOSV uses k1..k5 radial only and **ignores `p`** [11].
  PanoForge treats the four `ClipMeta` coefficients as Brown-Conrady [16]
  (wrong block). **[U]** until checked against Osmo 360 samples.
- Extrinsic: OpenOSV uses `d_lens = R(cam_extri_q)·d_body` [8].
  telemetry-parser composes the Euler triple as `Ry(yaw)·Rz(roll)·Rx(pitch−90°)`
  and inverts it. The other compositions cost >1° of relative roll, which shows
  as a 40 px vertical seam slide at 4096 wide [2]. **[C]** that the order
  matters; the exact conventions are **[U]** in Spherecut's frames.
- Translation is zero: DJI bakes stitch distance into the focal ladder [2].
- Blending: DJI uses lens 0 alone to `90° − 0.038·180°`, lens 1 alone beyond
  `90° + 0.038·180°`, and crossfades over **13.68°** around the seam. This is
  `seam_overlap_ratio = 0.038` from DJI's stitcher bundle [2].
  **[S]**, and DJI-derived (§11.2).

### 3.4 Calibration frame → coded stream (mode dependent)

| Mode | Mapping | Source |
|---|---|---|
| 8K (3840²) | 1:1 | [10] **[C]** with [2] |
| 6K (3000²) | **central 3776 px of the 3840 frame × 0.794492** (not 3000/3840). `c_stream = 1500 + (c_cal − 1920)·0.794492`. A plain 3000/3840 scale misaligns the seam by 4.25° | [8][10] **[S]**, verified by OpenOSV on 6K and 8K clips from two bodies |
| 4K (1920²) | derive from `digital_focal_length / fx`. Flagged unverified by OpenOSV itself | [8][10] **[U]** |
| LRF half (1024²) | 8K parent: 1024/3840 = 0.2666667. 6K parent: 0.2711864 (1024/3776) | [10] **[S]** |

**Conflict:** telemetry-parser maps calibration to picture with a plain
`scale(w/3840, h/3840)` and says the two "equal [each other] for every 360
body seen so far" [2]. That suggests it was only exercised on 8K clips and
would misplace 6K and 4K footage. **[U]**. This needs a 6K sample.

`digital_focal_length` is always 0.2764537 × the full-size clip's lens
width (829.3612 at 6K, 1061.5823 at 8K). An LRF repeats its parent's value
[10]. **[S]**

### 3.5 Masks

Each slot's `occlusion_pt_x/y` gives a body silhouette in calibration px (an
open contour across the bottom of the circle, its first point repeated) [2].
**[C]** ([2][9]) telemetry-parser closes it outward. It also ships bundled
"DJI Studio mask" polygons (`rig_mask_data.rs`) traced from DJI Studio's
`mask_fisheye_*.bin` files [5]. See §11.2 before reusing those.

## 4. Telemetry

### 4.1 Per-frame `FrameMeta` (Osmo 360 numbering)

| Path | Content | Source |
|---|---|---|
| `3.1` header | `frame_seq_num` (+2 per frame), `frame_timestamp` (µs since power-up), stream_id | [4][9] **[C]** |
| `3.2.3` / `.4` / `.6` | ISO (float), exposure time (rational), white balance (K) | [9][16][21] **[C]** |
| `3.2.9` `camera_attitude` | one quaternion per frame, equal to entry 4 of the fused batch | [9][16] **[C]** |
| `3.2.10` `camera_acc` | accelerometer, fields 2/3/4, float **g**, one per frame (norm ≈ 1) | [9][13][16][21] **[C]** |
| `3.2.16` | sensor temperature °C | [9][16] **[C]** |
| `3.3.2` `IMU_attitude_after_fusion` | `DeviceMultiAttitude { current_frame, prev_frame, next_frame }`, each a `DeviceAttitude { u32 timestamp, u32 vsync, repeated Quaternion attitude, float offset }` | [4][9][13] **[C]** |
| `3.4.2` `gimbal_frame_meta.gps_basic` | GPS (lat/lon/alt/time). ExifTool decodes it for oq101 | [4][21] **[C]** for the schema |

- **No raw gyro on the Osmo 360.** The attitude is the camera's own
  gyro+gravity fusion [1][13]. **[C]** telemetry-parser emits it as quaternion
  data, with no gyroscope vectors [1].
- **Rate:** `ClipMeta.10 imu_sampling_rate` = 1000 Hz [9]. The effective rate
  is ~995 Hz [8][16]. A packet holds about 1/fps of a second of readings:
  16–17 at 60 fps, ~33 at 30 fps, ~40 at 25 fps [8][13][16]. **[C]** Avata 360:
  4000 Hz, 66–67 per frame [7][13].
- **Timing pitfalls**, all from telemetry-parser's Osmo 360 work [1][3] **[S]**:
  - `DeviceAttitude.timestamp` is the low 32 bits of the µs clock. Unwrap it
    against the frame's own timestamp.
  - The flat-camera `offset` convention places samples ~25 ms early on the
    Osmo 360.
  - The stated 1000 Hz is not the true spacing. Fit a sliding least-squares
    grid of anchor against reading index (±25 packets), and split the grid at
    gaps.
  - The IMU clock drifts ~5 ms per 5 min against the frame clock.
- **Rolling shutter:** `ClipMeta.13 LRO_value` (ns per line) × `sensor_res`
  rows gives 18.30 ms on telemetry-parser's clip. `sensor_readout_time` would
  give 24.02 ms. DJI's 360 EIS uses the LRO figure [1]. spirula reports
  "24.0 ms readout" [13], and OpenOSV leaves the unit of `sensor_readout_time`
  unverified [9]. **[U]**. The value probably depends on the mode.
- **Attitude convention — disputed.** All agree the stored order is fields
  1–4 = (w,x,y,z) by name [4][9]. OpenOSV reads the floats as **XYZW,
  world→body**, relabels the axes by `(x,y,z)→(y,−z,−x)`, and validated this
  against lamp poles and sun elevation on 8K car clips [8][11]. telemetry-parser
  reads WXYZ, multiplies by a fixed `(0.5,−0.5,−0.5,0.5)`, and applies a Y-180°
  turn, the same path as its flat DJI cameras [1]. spirula found that "the
  quaternion maps sensor to world" by rotating the accelerometer [13]. These
  readings may be equivalent up to fixed frame changes. **[U]**. Settle it with
  sample #10 (tripod yaw/pitch/roll).
- **Accelerometer frame:** body frame, `(a_y, a_x, −a_z)` in OpenOSV's rig axes
  [8]. **[S]**
- **GPS:** the camera has no internal GPS [13][16]. **[C]** The schema has GPS
  fields [4][21], and a retailer lists DJI's Osmo Action GPS Bluetooth Remote
  as Osmo 360-compatible [34]. That the remote fills `gps_basic` is **[U]**.
- **Osmo 360 II (wa530 numbering):** adds `FrameMetaOfIMU.5 IMU_raw_attitude`.
  It holds a u64 first-sample timestamp plus repeated (gyro xyz rad/s, acc xyz
  g), stated 1000 Hz, measured 995 µs spacing, axes `zyx` in a 360 recording
  [1][3][4]. Field 5 was added to the proto by telemetry-parser's author and
  does not come from a DJI-shipped proto [4]. **[S]**
- **In-camera stabilisation:** `ClipMeta.9 eis_status` exists [4][9]. Whether
  OSV frames are ever pre-stabilised is **[U]** (sample #11).

### 4.2 Tool support status

- **telemetry-parser** (Rust; Gyroflow's parser): detects on `djmd` plus
  "CAM meta" and lists `osv` as an extension [1]. On Osmo 360 it outputs
  quaternions, exposure, the lens profile and a `CameraRig`: two lenses
  (video tracks 0/1), all calibration variants, masks, blend and readout
  [1][2]. **Git master only**: the crate reports 0.4.0, but crates.io has
  0.2.6 from 2022 [6][31]. PR #55 was tested on a single fw 10.00.25.29 clip
  (590 quaternion samples) [6]. Issue #52 (2026-03) answered Osmo 360 with
  "not for now… a lot of work". The support came afterwards [6].
- **Gyroflow** (1a78c95, 2026-10-22): pins telemetry-parser at 2817b42 but does
  **not** list `osv` in its file pickers. It does not consume `CameraRig`.
  Issue #1164 ("Support Osmo 360 as two fisheye lens cameras") is open [22].
  Gyroflow's lens-profile database has no Osmo 360 entries [23].
- **ExifTool** ≥13.39 (2025-10-15) decodes oq101 serial number, model,
  timestamp, ISO, shutter, colour temperature, GPS and accelerometer. 13.59
  added frame number [21]. `.OSV` is not in its extension table (13.59), but content detection
  works: PanoForge cross-checked its accelerometer values against
  `exiftool -ee` [16][21]. **Discrepancy:** ExifTool maps `1-14-1` to FNumber
  [21], while telemetry-parser and OpenOSV put `sensor_res` at ClipMeta.14
  and `f_number` at .15 [4][9]. **[U]**. ExifTool may be wrong for current
  firmware.

## 5. `.LRF` proxy

- Same stem as the `.OSV` (`CAM_YYYYMMDDhhmmss_NNNN_D.LRF`), in the same
  folder [16][19]. **[C]**
- One H.264 2048×1024 video track, the cover JPEG, and the same `djmd`
  metadata with its own `camd` [7][13][19]. **[C]**
- **Layout conflict:** OpenOSV says the frame holds **both lenses side by side**
  (left = slave, right = master, 1024² each) and backs this with seam-overlap
  NCC measurements and per-mode scale rules [7][10]. camera-toolkit calls it a
  "stitched equirectangular stream" [19]. **[U]**. OpenOSV's evidence is
  stronger. Check it with one LRF.
- The frame rate can be lower than the OSV's (25 fps for a 50 fps clip [7]; "30
  fps" in [13]). The LRF's `stts` is **not** trustworthy across dropped frames
  (it writes 60 ms gaps as 80 ms), so time it from `djmd` timestamps. The first
  capture timestamps of an OSV and its LRF are equal [7]. **[S]**
- Usable as Spherecut's editing proxy, mapped with the 1024-px scale rules in
  §3.4 [10]. **[S]**

## 6. Stitched exports (DJI Studio desktop / DJI Mimo app)

- Both apps stitch OSV to an equirect 2:1 MP4. DJI Studio is the desktop path,
  Mimo the phone path [30][32]. **[C]** spirula used "DJI Studio's equirect
  export" of an Avata 360 clip as ground truth [13]. Neither app runs on
  Linux.
- Export codec, bit depth, maximum resolution and **whether `sv3d`/`st3d` or the
  GSpherical XMP is written: not found in any source.** **[U]** One
  user workflow injects 360 metadata manually after colour-correcting a DJI
  Studio export [30]. That proves nothing, because re-encoding can drop it.
  A guide claims Mimo exports "16:9 equirectangular (with 360° metadata)" [32-b];
  16:9 contradicts equirect, so I disregard it.
- The OSV's own `covr` JPEG (688×344) is an in-camera stitched equirect
  [16][19]. **[C]** It is a cheap thumbnail and a sanity reference for lens
  orientation.
- **Spherecut fallback:** accept a 2:1 equirect MP4/MOV from DJI as equirect.
  Use the spherical metadata when present; when it is absent, use the aspect
  ratio plus the `Model`/encoder strings, and **ask the user** to confirm.
  The `djmd` track probably does not survive export. **[U]** (sample #12).

## 7. FFmpeg behaviour

- `libavformat/mov.c` (master 179d083) has no `djmd`/`dbgi`/`camd`/OSV-specific
  code. Metadata tracks surface as generic data streams, and `camd` is ignored
  [24]. **[C]** with [14][15].
- `tkhd` flags: only track 1 has the enabled bit, so FFmpeg gives only it
  `AV_DISPOSITION_DEFAULT` [7][24]. Naive `ffmpeg -i x.OSV` picks one lens.
  Spherecut must open both video streams explicitly.
- The `covr` JPEG appears as an MJPEG `attached_pic` stream. Do not treat it as
  a lens [14][15]. **[C]**
- No FFmpeg trac ticket about OSV was found (trac is unreachable from here;
  web search found none). **[U]**
- Baseline stitch without calibration:
  `hstack` + `v360=input=dfisheye:ih_fov=190:iv_fov=190:yaw=90` aligns
  "correctly vs the thumbnail" but shows parallax and seams [16]. It is fine
  as a smoke test and not as the product.
- Decoding two 3840² Main10 streams at 50 fps is ~1.5 Gpx/s. Hardware decode
  support at 3840² varies by GPU (not checked; **[U]**). The OSV player
  [20] reached ~38 fps at 8K30 with NVDEC on an RTX 3060 laptop. **[S]**

## 8. Other DJI-isms worth knowing

- File naming: `CAM_<timestamp>_<NNNN>_D.OSV`/`.LRF` [16][19]. Copies renamed to
  `_D-001.OSV` occur [10]. Recording-split (chapter) behaviour is **[U]**
  (sample #13). The storage file system is exFAT [26].
- Colour: D-Log M, HLG and Normal are selected via `color_mode` [9]. DJI
  publishes a D-Log M → Rec.709 LUT but no Osmo 360 HLG one [12]. **[S]**
- The second `djmd` track is a stripped copy. Read only the first, as
  telemetry-parser does (`single = true`) [1][7]. **[C]**

## 9. Discrepancies and open questions (each needs a real file)

| # | Question | Positions | Sample |
|---|---|---|---|
| Q1 | Is tangential `p` applied, and is the k3/k4 legacy fixup ever needed? | TP+spirula: yes / OpenOSV: ignores [2][11][13] | 8 |
| Q2 | Mapping from 6K/4K calibration to stream | OpenOSV crop 3776×0.7945 / TP plain scale [2][10] | 3, 4 |
| Q3 | Attitude quaternion order, sense and axes | OpenOSV XYZW-w2b-relabel / TP WXYZ·fixed [1][8][11] | 10, 9 |
| Q4 | Rolling-shutter readout per mode (LRO vs `sensor_readout_time`) | 18.30 vs 24.02 ms [1][13] | 1, 3, 4 |
| Q5 | LRF layout: side-by-side fisheyes or equirect | [7][10] vs [19] | any + its LRF |
| Q6 | Do DJI Studio/Mimo exports carry `sv3d`/GSpherical? | unknown | 12 |
| Q7 | Default slot (`far_11` vs `native_refine`); ladder semantics | [2][7][9] | 8 |
| Q8 | Which physical face is the master lens | unknown | 8 |
| Q9 | Any multichannel audio mode | none reported | 6/7 |
| Q10 | Are OSV frames ever pre-stabilised (`eis_status`)? | unknown | 11 |
| Q11 | ExifTool `1-14` FNumber vs `sensor_res` | [4][9] vs [21] | any |
| Q12 | Chaptering at file-size limits | unknown | 13 |
| Q13 | Firmware drift (field numbers, slot fill) | only fw 10.00.25.29 documented | note firmware |

## 10. Feasibility verdict

**Feasible: plan full raw OSV support for the Osmo 360, and keep DJI-export
ingest as a fallback.**

1. The container is plain ISO-BMFF. Both lens streams decode with stock FFmpeg
   HEVC 10-bit; Spherecut only needs to select both streams and pair them by
   sample index [7][24].
2. The geometry ships in every file, per unit, in a protobuf whose schema is
   public under MIT/Apache [4]. Independent implementations stitch or
   register from it: telemetry-parser, OpenOSV and spirula-studio, plus
   osv-to-pano on Avata 360 only. The best report ~1 px seam or lens-to-lens
   registration [2][7][13][18].
3. Motion ships as a ~1 kHz fused attitude. That is enough for stabilisation,
   horizon lock and rolling-shutter correction. It is arguably easier than raw
   gyro, because gravity is already fused in. The loss is gyro-bias estimation
   and custom fusion on the Osmo 360. The Osmo 360 II adds raw IMU [1][13].
4. What remains is **validation, not discovery** (§9). DJI-grade seams (flow
   seams, lens-protector curve, sky de-banding) are polish beyond a calibrated
   feather blend.
5. **Risks:**
   - The schema is undocumented by DJI and differs between bodies; mitigate by
     choosing the schema by product name with a layout fallback [2][7].
   - Firmware may move fields.
   - There is no internal GPS.
   - The license constraints in §11 apply.

**Suggested Spherecut shape:**
- `camera::dji` detects the format by a `djmd` track with "CAM meta" plus
  `product_name`, never by extension.
- Decode `djmd` with `prost`, using the `.proto` files from telemetry-parser
  (with attribution).
- Implement the lens model, extrinsics, per-mode scaling, the occlusion mask
  and quaternion slerp in Spherecut's own `stitch`/`motion` crates.
- Use telemetry-parser (pinned git rev) as a **test oracle**. Note that
  `deny.toml` currently has `unknown-git = "deny"` [33]: a git dependency
  needs an `allow-git` entry and a DECISIONS.md record. Vendoring only the
  `.proto` files avoids that.

**Samples needed** (numbers refer to `docs/SAMPLES.md`; put them in
`samples/dji-osmo360/`). Each `.OSV` **with its `.LRF`**, copied off the
card, with the firmware version in NOTES.txt:
- 1 (8K30)
- 2 (8K25/24)
- 3 (6K60, the key one for Q2)
- 4 (4K100)
- 5 (D-Log M; also one HLG clip for `color_mode` 9)
- 8 (seam: Q1, Q7, Q8)
- 9 and 10 (attitude: Q3, Q4)
- 11 (Q10)
- 12 (DJI Studio export of clip 1 **and** of clip 8, untouched: Q6 and the stitch reference)
- 13 if possible (Q12)

DJI-specific extras:
- one clip with **Lens Protection Mode on** (`extri_lens_mode` = 1);
- the camera's own "Mono"/"Stereo" audio setting noted;
- an Osmo 360 II clip if one is available.

## 11. Licenses and provenance of reusable code

### 11.1 License compatibility with GPL-3.0-or-later

| Project | What is reusable | License | Compatible? |
|---|---|---|---|
| telemetry-parser [1–6] | Rust DJI parser, `.proto` schemas, rig/timing logic | MIT OR Apache-2.0 (Cargo.toml, SPDX headers) | Yes |
| video-types [31] | geometry descriptors used by telemetry-parser's rig | MIT OR Apache-2.0 | Yes |
| mp4parse-patched, prost, fc-blackbox-patched [31] | telemetry-parser deps | MPL-2.0; Apache-2.0; MIT/Apache | Yes (all on the deny.toml allow-list [33]) |
| OpenOSV [7–12] | C++ decoder, geometry, docs | Apache-2.0 (+NOTICE) | Yes (one-way). Keep the NOTICE if code is ported |
| spirula-studio [13] | measurements and notes (C++) | GPL-3.0 (badge/LICENSE; -only vs -or-later not checked) | Yes. Check "or later" before copying code |
| osv2mkv [15] | Rust ISO-BMFF remuxer for OSV | MIT | Yes |
| osv2mov [14], PanoForge [16], osv-to-pano [18], osvsharp | Go/Python/C tools | MIT | Yes |
| OSVToolbox [17] | C++ extract/recompose | **Conflicting:** LICENSE file is GPL-3.0, README says MIT | Yes either way. dji-osv2mp4 (MIT) bundles a copy |
| ExifTool [21] | Perl tag tables | Perl terms (Artistic or GPL) | Reference only |
| files_360viewer | PHP/TS viewer | AGPL-3.0-or-later | Avoid copying |
| osmo360-player, OSMO360-Boost-Video-Viewer | C++/Python | **No license file** (all rights reserved) | Do not copy |
| dual-fisheye-frame-extractor | Python | README says MIT, no LICENSE file | Reference only |

### 11.2 Provenance cautions (not legal advice)

- **DJI-authored protos.** `dvtm_library.proto` in telemetry-parser carries
  DJI's own header and comments, for example "lightCut app/gyroflow need to
  know this value… in our model, fx equals to fy". It has no license header of
  its own, so it was apparently written by DJI and is redistributed under the
  repo's MIT/Apache terms [4]. The risk looks low (it describes an interface),
  but record it. **[U]**
- **DJI-derived data.** telemetry-parser's `rig_mask_data.rs` holds outlines
  traced from DJI Studio 1.0.0.28782's bundled `mask_fisheye_*.bin` files
  (SHA-256 listed in the file) [5]. The blend constant 0.038 and the default
  `far_11` slot come from DJI's stitcher and from hooking DJI Studio at runtime
  [2]. OpenOSV deliberately excludes DJI masks and LUTs, re-implements numeric
  facts (195.18° FOV, lens-protector curve), and documents each one [12].
  **Recommendation:**
  - Use the per-clip `occlusion_pt` polygons from the file.
  - Use Spherecut's own blend width.
  - Do **not** embed `rig_mask_data.rs`.
  - Record in DECISIONS.md any DJI-derived constant that is adopted.
- The `docs/360/DJI*.md` notes that telemetry-parser cites
  (`oxivideo/docs/360/…`) are not in the public `AdrianEddy/oxivideo` repo
  (checked branch `legacy`) [2][5]. **[U]**

## Sources

All repositories were read at the commit shown, on 2026-10-22.

1. telemetry-parser `src/dji/mod.rs` @ 2817b42, https://github.com/AdrianEddy/telemetry-parser/blob/2817b42/src/dji/mod.rs
2. telemetry-parser `src/dji/rig.rs` @ 2817b42
3. telemetry-parser `src/dji/timing.rs` @ 2817b42
4. telemetry-parser `src/dji/dvtm_oq101.proto`, `dvtm_eagle4_wa530.proto`, `dvtm_library.proto` @ 2817b42, and commit 54c85e8 (2026-10-08, adds `IMU_raw_attitude`)
5. telemetry-parser `src/dji/rig_body_masks.rs`, `rig_mask_data.rs` @ 2817b42
6. telemetry-parser PR #55 (commit 421b4ab, 2026-06-09), https://github.com/AdrianEddy/telemetry-parser/pull/55; issue #52, https://github.com/AdrianEddy/telemetry-parser/issues/52; README; Cargo.toml (version 0.4.0, license)
7. OpenOSV `docs/FORMAT.md` @ d1c657a, https://github.com/Kemerd/OpenOSV
8. OpenOSV `docs/GEOMETRY.md` @ d1c657a
9. OpenOSV `proto/dvtm_osmo360.proto` @ d1c657a
10. OpenOSV `src/osv/geom/StreamScaling.cpp`, `include/osv/meta/FormatInfo.h`, `CHANGELOG.md` @ d1c657a
11. OpenOSV `include/osv/geom/KannalaBrandt5.h`, `src/osv/geom/KannalaBrandt5.cpp`, `src/osv/geom/AttitudeTrack.cpp` @ d1c657a
12. OpenOSV `NOTICE`, `docs/LEGAL.md`, `docs/COLOR.md`, `LICENSE` @ d1c657a
13. spirula-studio `docs/notes/imu-gps-for-sfm.md` §2.3 @ 9a24f09, https://github.com/harry7557558/spirula-studio
14. osv2mov `README.md`, `main.go`, `LICENSE` @ a54718a, https://github.com/yoshihiro0323/osv2mov
15. osv2mkv `README.md`, `src/media.rs`, `LICENSE` @ 36d5652, https://github.com/djkcyl/osv2mkv
16. PanoForge `SPEC.md`, `tools/NOTES-djmd.md`, `tests/fixtures/calibration.json`, `LICENSE` @ 5bcfbe8, https://github.com/Belenos-Toutatis/PanoForge
17. OSVToolbox `README.md`, `LICENSE` @ f815119, https://github.com/ChelouteVR/osvtoolbox
18. osv-to-pano `docs/osv-format.md` (Avata 360), `LICENSE` @ 4832bb0, https://github.com/zekepari/osv-to-pano
19. camera-toolkit `Sources/CameraToolkitCore/Media/DJI360Media.swift` @ 3530b60, https://github.com/nsoliven/camera-toolkit
20. osmo360-player `README.md` @ df73201, https://github.com/ademets/osmo360-player
21. ExifTool 13.59 `lib/Image/ExifTool/DJI.pm` (GitHub mirror 2200871); `Changes` for 13.39 (a67d1ed, 2025-10-15), https://github.com/exiftool/exiftool
22. Gyroflow @ 1a78c95: `src/core/Cargo.toml`, `src/ui/App.qml`, `src/ui/menu/MotionData.qml`, `src/core/util.rs`; issue #1164, https://github.com/gyroflow/gyroflow/issues/1164
23. gyroflow/lens_profiles @ 886fc61, `DJI/` directory, https://github.com/gyroflow/lens_profiles
24. FFmpeg `libavformat/mov.c` @ master 179d083 (fetched 2026-10-22), https://github.com/FFmpeg/FFmpeg
25. Phototrend, "DJI Osmo 360: dual 1-inch sensors", 2025-07-31, https://phototrend.fr/en/2025/07/dji-osmo-360-dual-1-inch-sensors/
26. Cathay Photo, Osmo 360 spec sheet (retailer copy of DJI specs), https://www.cathayphoto.com.sg/cameras/vr-360-cameras/dji-osmo-360-standard-combo
27. DroneDJ, Osmo 360 launch date, 2025-07-24, https://dronedj.com/2025/07/24/dji-osmo-360-camera-launch/
28. Fstoppers, "DJI Launches the Osmo 360 II", https://fstoppers.com/news/dji-launches-osmo-360-ii-flagship-360-degree-camera-new-1-inch-hdr-sensor-and-904446
29. DJI support FAQ, Osmo 360 ("records panoramic videos in OSV format"), https://repair.dji.com/help/content?customId=01700043553&lang=en&paperDocType=ARTICLE&re=US&spaceId=17
30. xaionaro-go/djictl issue #3 (DJI Studio export workflow), https://github.com/xaionaro-go/djictl/issues/3
31. crates.io API: `telemetry-parser`, `video-types`, `mp4parse-patched`, `fc-blackbox-patched`, `prost` (versions and licenses, queried 2026-10-22)
32. mirame360, "DJI Osmo 360: Formats and Workflow" (resolutions, AAC, 4 mics), https://mirame360.com/docs/cameras/dji-osmo-360. 32-b: studiosport.fr export guide, https://www.studiosport.fr/guides/dji-osmo-360/export-video.html
33. Spherecut `deny.toml` (local, `[sources] unknown-git = "deny"`; license allow-list)
34. Retailer listing, "Osmo Action GPS Bluetooth Remote Controller for DJI Osmo 360, …" (compatibility claim only), https://systep.cl/products/original-osmo-action-gps-bluetooth-remote-controller-for-dji/212370702/
