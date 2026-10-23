# GoPro MAX / MAX 2 (`.360`): format notes

Researched 2026-10-22. Sources are cited by number and listed at the end, with
repository, commit and file path (and line numbers where they help).

**What was actually examined.** No GoPro hardware was available, and no MAX 2
file. The note rests on three kinds of evidence:

- **Real GoPro MAX pixels.** max2sphere's test frames: track 0 and track 5 of
  `GS018421.360` (5.6K) and `GS018423.360` (3K), extracted to JPEG with FFmpeg,
  plus GoPro Player's stitched equirect of the same frame [3]. These were
  measured for this note.
- **Real GoPro MAX files.** gpmf-parser's `samples/max-360mode.mp4` and
  `samples/max-heromode.mp4` (firmware H19.03.01.30.00) [2]. Their `udta`
  header metadata and GPMF telemetry were parsed for this note.
- **Third-party logs and code.** Two FFmpeg stream listings of real `.360`
  files, one camera-original [17] and one from the GoPro cloud [18], plus the
  source code of every open implementation found.

Confidence tags:

- **[M]** Measured for this note on real GoPro MAX pixels or files (see §2.8 and §4.2).
- **[C]** Corroborated: two or more independent sources agree.
- **[S]** Single source: one project says it and nothing contradicts it.
- **[U]** Unverified, inferred, hearsay or **conflicting**. Treat it as a hypothesis (see §7).

## 0. Summary

| Question | Answer |
|---|---|
| Container | QuickTime/MP4 (`ftyp mp41`), extension `.360`. FFmpeg demuxes it without special handling [17][18]. **[C][M]** |
| Video | **Two HEVC (`hvc1`) video tracks of identical size**, each holding three cells of an equi-angular cubemap (EAC). MAX: 4096×1344 per track (5.6K modes) or 2272×736 (3K) [3][4][17][18][20]. MAX 2: 5952×1920 (10-bit) or 5888×1920 (8-bit) [11][12]. **[C]** for MAX, **[S/U]** for MAX 2 |
| Projection | `PRJT = "EACO"`: **split, overlapping EAC** [1][5]. Track 0 holds LEFT·FRONT·RIGHT, upright. Track 1 holds DOWN·BACK·UP, each turned a quarter turn. The four side cells are cut at their centre line (the lens seam) and store **two registrations** that overlap by `2·overlap` stored pixels. The geometry comes from `PMOD = [overlap, pre, post]` in the `udta` header [5]. **[M][C]** |
| Overlap | MAX 5.6K: `PMOD = [32, 1408, 1376]`, so the registrations are offset by 62.55 px. **I measured 62.8 px** (§2.8). Not 32 px, and not exactly max2sphere's 64 [3]. **[M]** MAX 2: `[48, 2016, 2016, ?]` with no squeeze [5]. **[S]** |
| Audio (MAX) | AAC-LC stereo 48 kHz ("GoPro AAC"), plus **4-channel 32-bit PCM `in32` ("GoPro AMB")**. FFmpeg reads the AMB track as `ambisonic 1`, which with FFmpeg's SA3D parser means **first-order ACN/SN3D (AmbiX)**, channel order W, Y, Z, X [8][18]. **[C]**, though the physical orientation is not verified |
| Telemetry | GPMF in a `gpmd` track ("GoPro MET"). Streams: GYRO ≈790 Hz, ACCL ≈197 Hz, MAGN ≈24 Hz, plus per-frame CORI/IORI/GRAV/SHUT/ISOE, a compressed per-frame DISP, and GPS5 ≈18 Hz [1][2]. **[M]** |
| Header metadata | A GPMF blob in `moov/udta/GPMF`: `MINF`, `PRJT`, `PMOD`, `PTCL`, `EISE`/`EISA`, `SROT`, and the lens calibration DEVCs [1][2][5]. **[M][S]** |
| Proxy `.LRV` | MAX: 1408×704 H.264, **dual fisheye** (`PRJT = "FSFB"`), with the same GPMF telemetry [2][17]. **[M]** MAX 2: unknown. **[U]** |
| telemetry-parser | Builds the `.360` rig (`src/gopro/eac.rs`) on **git master only** (0.4.0). The newest crates.io release is 0.2.6 (2022) [5][7]. **[C]** |
| Licences | gpmf-parser (Apache-2.0 OR MIT), max2sphere (Apache-2.0), telemetry-parser (MIT OR Apache-2.0): **all compatible with GPL-3.0-or-later** (§6). **[C]** |
| Verdict | **Full support without any closed SDK is feasible.** The MAX geometry is pinned to the pixel. The MAX 2 and the time-sync details need the sample files in §7. |

## 1. Container

### 1.1 Identification

- Extension `.360`; file names `GSccnnnn.360`, where `cc` is the chapter and
  `nnnn` the clip number [19]. **[S]**
- MP4 with `major_brand mp41` [2][18]. Video handler names are
  `"GoPro H.265"` and the encoder string is `"GoPro H.265 encoder"` [17][18].
  **[C]**
- The definitive test is the `udta` GPMF: `DEVC "Global Settings"` with
  `PRJT = "EACO"` [1][5]. `MINF` holds the model (`"GoPro Max"` in [2]). For
  comparison, a HERO-mode file has `PRJT = "GPRO"` and the low-res fisheye file
  has `"FSFB"` [2]. **[M]**
- **No Google spherical metadata** (`sv3d`/`st3d`) was seen: neither FFmpeg
  listing shows spherical side data [17][18]. Spherecut has to recognise the
  file itself. **[S][U]** It needs a real file to confirm.

### 1.2 Track layout

Camera-original MAX `.360` in a video mode. The order is pieced together
from four sources, which agree: [17] (a 25 fps PAL clip whose listing shows
indices 0, 5 and 6), [3] (video at 0 and 5), [14] (fixtures README: GPMF "always
index 3" on MAX) and [12] (a fixture modelled on a real `GS010013.360`).
The LRV-like file [2] has the same order for indices 0–4. **[C]**

| ffprobe index | Track ID | Handler | Sample entry | Content |
|---|---|---|---|---|
| 0 | 1 | `GoPro H.265` | `hvc1` | **Video track 0**: LEFT, FRONT, RIGHT |
| 1 | 2 | `GoPro AAC` | `mp4a` | AAC-LC, 48 kHz, stereo, about 189 kb/s |
| 2 | 3 | `GoPro TCD` | `tmcd` | timecode |
| 3 | 4 | `GoPro MET` | `gpmd` | GPMF telemetry |
| 4 | 5 | `GoPro SOS` | `fdsc` | GoPro internal. FFmpeg discards it because its samples have zero duration [2] |
| 5 | 6 | `GoPro H.265` | `hvc1` | **Video track 1**: DOWN, BACK, UP |
| 6 | 7 | `GoPro AMB` | `in32` | 4-channel ambisonic PCM (s32le), 48 kHz, 6144 kb/s |

Variants:

- **TimeWarp** files have no audio, so the second video track is index 4,
  not 5 [3] (README lines 75–76). **[S]**
- **GoPro cloud "concat" downloads** reorder and drop tracks: video, video,
  AAC, AMB, tmcd, gpmd, with no SOS track, and a 90 kHz video timescale [18].
  **[S][M]**
- **MAX 2**: a stereo track and a 4-channel ambisonic track exist [11]
  (README line 95). One project puts the AMB track at index 5 as `pcm_s24le`
  [14]. That index differs from the MAX, so the track order may differ too.
  **[U]**

**Rule: never hard-code indices.** Take the two `vide` tracks with
`hvc1`/`hev1` sample entries in `moov` order. The first is track 0 (top
strip) and the second is track 1 (bottom strip). Cross-check the handler
names. This is what max2-reframe-resolve [11] (`src/source360.cpp` lines
168–212) and gopromax-conversion-tools [10] (commit 0072283) do. **[C]**

### 1.3 Video tracks

| Model / mode | Per-track size | Cells (side, centre, side) | Frame rates | Codec |
|---|---|---|---|---|
| MAX 5.6K | 4096×1344 | 1376, 1344, 1376 | 23.976/24, 25, 29.97/30 [4][17][18] | HEVC Main 8-bit, `yuvj420p`, full range, BT.709 tags [17][18] |
| MAX 3K | 2272×736 | 768, 736, 768 | 60 (50?) [4] | as above |
| MAX 2 10-bit | 5952×1920 | 2016, 1920, 2016 [5] | **unknown** | HEVC Main10, decoded as P010 [11][12] |
| MAX 2 8-bit | 5888×1920 | 1984, 1920, 1984 [11] | **unknown** | HEVC Main [12] |

- Both tracks have the same size, timescale and frame rate in every listing
  [17][18]. **[C]**
- Bit rate: about 30 Mb/s per track at 5.6K, about 60 Mb/s combined [18][20].
  **[C]**
- GoPro describes the 5.6K packing as 4032×1344 per track, "increased by an
  additional 64 pixels" for the stitch overlap [20], which gives 4096 [C]. The
  same page calls EAC an "Equal Area Cubemap", but the mapping is
  equi-angular (§2.5).
- **Colour.** Full-range (`pc`) BT.709 [17][18]. max2-reframe-resolve
  converts to RGB with full-range BT.709 [11] (`src/reframe_kernel.h` line
  148). **[C]** Protune "Flat" is a picture profile, not a log curve.
  GP-Log exists on the MAX 2 (§3).
- **Rolling shutter.** GoPro applies "Electronic Rolling Shutter (ERS)
  compensation in camera before encoding" [20]. **[S]** Do not apply RS
  correction to `.360` pixels by default. The header still carries
  `SROT` = 20.9 ms (sensor read-out time) [2]. **[M]**
- 10-bit detection: read `bitDepthLumaMinus8` from the `hvcC` record (byte 17
  of the record in [11] `src/mp4.cpp` lines 80–82), or take the decoder's
  pixel format. **[S]**

### 1.4 Audio

- **Stereo:** AAC-LC, 48 kHz, 2 ch [17][18]. **[C]**
- **Ambisonic (MAX):** `in32` sample entry, 32-bit little-endian PCM,
  4 channels, 48 kHz [17][18]. FFmpeg (version not logged) reports `ambisonic 1`
  on the cloud file [18], while an older FFmpeg guessed the layout as `4.0` [17].
  - FFmpeg's `mov_read_SA3D` accepts only `ambisonic_type 0`,
    `channel_ordering 0` (ACN) and `normalization 0` (SN3D). It rejects
    anything else with a warning, and then no ambisonic layout is reported [8]
    (`libavformat/mov.c` lines 9303–9385).
  - So an `ambisonic 1` report implies an SA3D box that declares
    **first-order AmbiX: ACN channel order W, Y, Z, X with SN3D
    normalisation**. **[C]**, by code reading plus a log line.
  - **Not verified:** whether the camera-original file (not only the cloud
    remux) carries SA3D, and whether the sound field's +X/+Y match the video
    frame of §2.3. Sample #6 in §7 settles both.
- **Ambisonic (MAX 2):** `pcm_s24le`, 4 ch, 48 kHz, AmbiX W, Y, Z, X, at
  stream index 5 [14] (`crates/vr180-pipeline/src/audio.rs` lines 1–70).
  **[S][U]**
- Spherecut should read SA3D itself if present, and otherwise default to
  AmbiX for a 4-channel track whose handler is `GoPro AMB`. **[U]**

### 1.5 Timecode

- A `tmcd` track ("GoPro TCD"). FFprobe shows it as `timecode: 15:44:03:21` on
  each track [2][18]. **[M][C]**
- max2-reframe-resolve reads sample 0 of the `tmcd` track as a big-endian u32
  start frame number [11] (`src/source360.cpp` lines 199–202). **[S]**
- Use it only for display and conform. Frame timing comes from the video
  `stts`.

### 1.6 Metadata locations

- **Telemetry:** the `gpmd` track. It has one sample ("payload") per about
  1.001 s; the last payload is longer (1.126 s in [2]) [1][2]. The MP4
  `stts`/`stsz`/`stco` of this track give each payload's time and position [1]
  (README "MP4 structure"). **[M][C]**
- **Header/settings:** a GPMF blob in `moov/udta/GPMF`. It holds DEVCs named
  `Global Settings`, `Geometry Calibrations`, `Back Lens`, `Front Lens` and
  `Highlights` [2], and telemetry-parser reads it with `src/gopro/udta.rs` [5].
  **[M]** The `PRJT`/`PMOD`/`MINF` fields that define the `.360` geometry live
  here (§2.2).

### 1.7 Decoding the two tracks in lockstep

The two video tracks are one picture, so every output frame needs frame N of
both tracks.

1. **One demuxer, two decoders.** Open the file once, route packets of the two
   video stream indices to two HEVC decoders (VA-API/Vulkan Video/software),
   and drop the other streams or route them to their own consumers. Packets of
   the two tracks are interleaved in `mdat`, so one reader avoids double I/O.
2. **Pair frames by presentation timestamp, not by decode order.** Both tracks
   share a timescale and frame rate [17][18]. Keep a small queue per track and
   emit a pair when the PTS match.
   - Whether the two tracks have identical `stts`, sample count and sync-sample
     (`stss`) tables is not verified [U]. At open time, compare `stts` and
     `stss`.
   - On a mismatch, fall back to nearest-PTS pairing and log it.
3. **Seeking.** Seek each track to its own sync sample at or before the
   target, then decode forward to the target PTS. Do not assume the GOPs line
   up.
4. **Resolution.** 4096×1344 Main is within every HEVC hardware decoder.
   5952×1920 Main10 needs a decoder that supports widths above 4096 (most
   current GPUs allow up to 8192). Fall back to software HEVC otherwise. This
   is general HEVC knowledge, not GoPro-specific. **[U]**
5. **Per-frame metadata.** CORI, IORI, GRAV, SHUT and ISOE have one sample per
   video frame [1][2] (§4.4). How to align them with frame N is an open
   question (§7).

### 1.8 Chapters

- Recordings split at about 4 GB into `GS01nnnn.360`, `GS02nnnn.360` and so on.
  At 5.6K that is about 8.5 minutes per chapter [19]. **[S]**
- Each chapter is a complete file. Join chapters by `nnnn`, and check the
  continuity of the timecode and telemetry.

## 2. The EAC layout (`PRJT = "EACO"`)

### 2.1 Overview

```
track 0 (W x H):  | LEFT (-X)  split | FRONT (+Z) | RIGHT (+X) split |   upright
track 1 (W x H):  | DOWN (-Y)  split | BACK  (-Z) | UP    (+Y) split |   quarter-turned
                  |<---- post ------>|<--- H ---->|<---- post ------>|   W = 2*post + H
                          ^ lens cut at x0 + post/2 (centre line of each side cell)
```

- The camera frame here is **+X right, +Y up, +Z forward**. FRONT is the
  hemisphere centre of one lens and BACK the other's. Which physical lens
  (screen side or not) is FRONT is **[U]**.
- The lens seam is the plane z = 0. The four side faces straddle it. FRONT
  and BACK each come from one lens only [4][20].
- Inside a side cell, the left half holds one lens's image of the face and the
  right half holds the other lens's image.
  - Each half extends `overlap` capture texels past the cut, so the content at
    the cut appears twice, offset horizontally by `2·O` stored pixels.
  - GoPro keeps this overlap "so blending can happen in post" [20].
  - The camera stitches the geometry, but the halves still need a crossfade.
  - Sampling only one half and stopping at the cut leaves a tear: `2·O` is
    about 4.3° on a MAX [5].

### 2.2 Parameters (`PMOD`)

`PMOD` is a u32 array in the `Global Settings` DEVC of the `udta` GPMF [5]
(`src/gopro/eac.rs` lines 62–66). Its fields are:

| Field | Meaning |
|---|---|
| `overlap` | `PMOD[0]`: overlap texels past the cut, at capture scale |
| `pre` | `PMOD[1]`: face plus both overlaps before horizontal squeeze. Vendor name `facePlusOverlapPreScale`, quoted in [5] |
| `post` | `PMOD[2]`: stored side-cell width (`facePlusOverlapPostScale`) |
| `PMOD[3]` | MAX 2 only. Meaning unknown [5][13] **[U]** |

The relations that hold are `pre == H + 2·overlap` and
`W == 2·post + H`, where W and H are the coded size of one track. telemetry-parser
refuses files that break them [5] (lines 147–156). Derived values:

```
k = post / pre          horizontal squeeze of side cells
S = H * k               stored width of a side face's full 90 degrees
O = overlap * k         stored overlap past the cut; registrations are 2*O apart
```

| Mode | W×H | PMOD | k | S | 2·O | Status |
|---|---|---|---|---|---|---|
| MAX 5.6K | 4096×1344 | [32, 1408, 1376] | 0.97727 | 1313.45 | 62.55 | Values from [5]. Lag **measured 62.8** [M]. Bourke: "32 pixels on each side of the central lines" [4]. **[C][M]** |
| MAX 3K | 2272×736 | not seen. max2sphere implies [16, 768, 768] [3] | 1 | 736 | 32 | Lag **measured 30.3**, which does not fit cleanly. **[U]**, so read PMOD |
| MAX 2 10-bit | 5952×1920 | [48, 2016, 2016, ?] [5] | 1 | 1920 | 96 | telemetry-parser measured a 96 px lag (ZNCC 0.74/0.78/0.60) [5]. **[S]** |
| MAX 2 8-bit | 5888×1920 | not seen. Implied [32, 1984, 1984, ?] | 1 | 1920 | 64 | From [11] `src/reproject.cpp` lines 13–16 (`ovl = (W-3H)/2`) and [12]. **[S][U]** |

**Implementation rule:** always derive the geometry from `PMOD` and the coded
size. If `PMOD` is missing, fall back to the table above by (W, H), and warn.

Equivalent equirect resolution at the side faces is `4·S` wide, and `4·H` at
FRONT/BACK:

- MAX 5.6K: 5376 px. This matches GoPro's "5.6K = 5376×2688" [20] and
  max2sphere's `equiwidth` [3].
- MAX 3K: 2944 px [3].
- MAX 2: 7680 px, which matches the "8K" label. That label itself is
  marketing **[U]**.

### 2.3 Face table

Each row gives where a face is stored and the unit vectors of its stored
image's axes in the camera frame. This is the table of max2-reframe-resolve
[11] (`proto/eac.py` lines 24–31), fitted there to GoPro Player exports of
MAX 2 files. It is also the table telemetry-parser uses for MAX and MAX 2
[5] (lines 100–109 and 159–168, with +Y down in its frame). **I verified it
on real MAX frames (§2.8). [M][C]**

| Face | Track | Cell (slot) | forward `f` | image-right `r` | image-down `d` | Lens halves (first ┊ second) |
|---|---|---|---|---|---|---|
| LEFT | 0 | 0: `[0, post)` | −X | +Z | −Y | back ┊ front |
| FRONT | 0 | 1: `[post, post+H)` | +Z | +X | −Y | front only |
| RIGHT | 0 | 2: `[post+H, W)` | +X | −Z | −Y | front ┊ back |
| DOWN | 1 | 0 | −Y | −Z | −X | front ┊ back |
| BACK | 1 | 1 | −Z | +Y | −X | back only |
| UP | 1 | 2 | +Y | +Z | −X | back ┊ front |

- A stored pixel at face coordinates (s, t) ∈ [0, 1]² (s rightwards, t
  downwards) looks along `f + tan(π/2·(s−½))·r + tan(π/2·(t−½))·d`.
- The "lens halves" column follows from the cut at z = 0. For example, in
  LEFT `r = +Z`, so `s < ½` means z < 0, which is the back lens.
- Track 1 faces are stored a quarter turn clockwise. On BACK, the upright
  "up" (+Y) points to image-right. telemetry-parser describes this as "bottom
  strip ... at one clockwise quarter turn" [5].
- This matches FFmpeg's YouTube EAC rotation table: 0/0/0 on the top row and
  270/90/270 on the bottom row, in FFmpeg's own face bases [8]
  (`vf_v360.c` lines 2617–2666). FFmpeg reproduces my sampler on a
  de-overlapped canvas (§2.8). **[M]**

### 2.4 Side cells: two registrations

For a side cell starting at `x0` (0 for slot 0, `post + H` for slot 2):

```
registration A (first half):   x = x0 + s*S           valid for s in [0, 1/2 + O/S]   (x < x0 + post/2)
registration B (second half):  x = x0 + 2*O + s*S     valid for s in [1/2 - O/S, 1]   (x >= x0 + post/2)
overlap band: s in [1/2 - O/S, 1/2 + O/S]   (= 2*O stored px, 4.29 deg on MAX, 4.5 deg on MAX 2)
```

Check for MAX 5.6K: `x0 + post/2 = x0 + 688 = x0 + S/2 + O` (656.73 + 31.27),
and registration B reaches `x0 + 2·O + S = x0 + 1376 = x0 + post` at s = 1.

Blending:

- A linear crossfade across the band (`w` from 0 to 1) is what max2sphere [3],
  max2-reframe-resolve [11] and Immuch360 [12] do.
- telemetry-parser reports that GoPro Player instead melds the halves with a
  Laplacian pyramid [5] (lines 118–121). **[S]**
- Linear is a good first implementation. Multiband blending is a later
  quality option.

### 2.5 Direction → texel (the math)

Input: a unit direction `dir` in the camera frame (+X right, +Y up, +Z
forward). Output: a colour.

1. **Face.** Pick the table row with the largest `dot(dir, f)`. That is the
   dominant axis and its sign.
2. **Gnomonic coordinates.** `a = dot(dir, r) / dot(dir, f)` and
   `b = dot(dir, d) / dot(dir, f)`. Both lie in [−1, 1].
3. **Equi-angular.** `s = ½ + (2/π)·atan(a)` and `t = ½ + (2/π)·atan(b)`.
   This is max2sphere's `q = atan(q)·4/π` mapped to [0, 1] [3] (lines
   336–390).
4. **Row.** `y = t·H`. Coordinates are continuous, with pixel centres at
   i + 0.5.
5. **Centre slot.** `x = post + s·H`.
6. **Side slot.** Sample both registrations and blend:
   - `xA = min(x0 + s·S, x0 + post/2 − 0.5)`
   - `xB = max(x0 + 2·O + s·S, x0 + post/2 + 0.5)`
   - `w = clamp((s − ½)·S/(2·O) + ½, 0, 1)`
   - `colour = mix(T[xA, y], T[xB, y], w)`

   The clamps keep a bilinear tap from straddling the hard cut. Outside the
   band, `w` is 0 or 1, so only one fetch is needed.
7. Sample track `i / 3` at (x / W, y / H). Clamp to the cell edges if you use
   mipmaps.

### 2.6 WGSL-style sketch (not compiled)

```wgsl
struct GoProEac {
  w: f32, h: f32,          // coded size of ONE track
  post: f32,               // PMOD[2]
  k: f32,                  // PMOD[2] / PMOD[1]
  overlap: f32,            // PMOD[0]
};
const PI = 3.14159265358979;
// rows: LEFT FRONT RIGHT (track 0), DOWN BACK UP (track 1); frame +X right, +Y up, +Z forward
var<private> FWD: array<vec3f, 6> = array(vec3f(-1,0,0), vec3f(0,0,1), vec3f(1,0,0), vec3f(0,-1,0), vec3f(0,0,-1), vec3f(0,1,0));
var<private> RGT: array<vec3f, 6> = array(vec3f(0,0,1),  vec3f(1,0,0), vec3f(0,0,-1), vec3f(0,0,-1), vec3f(0,1,0), vec3f(0,0,1));
var<private> DWN: array<vec3f, 6> = array(vec3f(0,-1,0), vec3f(0,-1,0), vec3f(0,-1,0), vec3f(-1,0,0), vec3f(-1,0,0), vec3f(-1,0,0));

fn face_of(d: vec3f) -> u32 {
  let a = abs(d);
  if (a.x >= a.y && a.x >= a.z) { return select(0u, 2u, d.x > 0.0); } // LEFT / RIGHT
  if (a.z >= a.y)               { return select(4u, 1u, d.z > 0.0); } // BACK / FRONT
  return select(3u, 5u, d.y > 0.0);                                   // DOWN / UP
}

fn fetch(track: u32, x: f32, y: f32, g: GoProEac) -> vec4f {
  let uv = vec2f(x / g.w, y / g.h);
  if (track == 0u) { return textureSampleLevel(track0, lin_clamp, uv, 0.0); }
  return textureSampleLevel(track1, lin_clamp, uv, 0.0);
}

fn gopro_eac(dir: vec3f, g: GoProEac) -> vec4f {
  let i = face_of(dir);
  let fz = dot(dir, FWD[i]);
  let s = 0.5 + atan(dot(dir, RGT[i]) / fz) * (2.0 / PI);
  let t = 0.5 + atan(dot(dir, DWN[i]) / fz) * (2.0 / PI);
  let track = i / 3u;
  let slot = i % 3u;
  let y = clamp(t * g.h, 0.5, g.h - 0.5);
  if (slot == 1u) {
    return fetch(track, clamp(g.post + s * g.h, g.post + 0.5, g.post + g.h - 0.5), y, g);
  }
  let x0  = select(g.post + g.h, 0.0, slot == 0u);
  let S   = g.h * g.k;
  let O   = g.overlap * g.k;
  let cut = x0 + 0.5 * g.post;
  let xa  = clamp(x0 + s * S,           x0 + 0.5,  cut - 0.5);
  let xb  = clamp(x0 + 2.0 * O + s * S, cut + 0.5, x0 + g.post - 0.5);
  let wgt = clamp((s - 0.5) * S / (2.0 * O) + 0.5, 0.0, 1.0);
  if (wgt <= 0.0) { return fetch(track, xa, y, g); }
  if (wgt >= 1.0) { return fetch(track, xb, y, g); }
  return mix(fetch(track, xa, y, g), fetch(track, xb, y, g), wgt);
}
```

Notes for the real shader:

- If the tracks stay as NV12/P010 planes, sample luma and chroma separately
  with the same normalised coordinates. Keep chroma taps one chroma texel away
  from the cut, then convert full-range BT.709 YUV to RGB (§1.3).
- Use LOD 0, or per-cell textures, near the cut. A mip level would average
  across the seam.

### 2.7 Texel → direction (inverse, for masks and tests)

```
slot = 0 if x < post, 1 if x < post + H, else 2;   t = y / H
centre:  s = (x - post) / H
side:    u = x - x0;  s = u / S  if u < post/2  (A)   else  s = (u - 2*O) / S  (B)
dir = normalize(f + tan(PI/2*(s - 1/2))*r + tan(PI/2*(t - 1/2))*d)
```

In half A, s can exceed ½, by up to O/S. That is the part of one lens's image
that lies past the seam.

### 2.8 Verification done for this note **[M]**

The frames are the real MAX frames of [3]: 4 frames of `GS018421.360` (5.6K)
and 4 of `GS018423.360` (3K), as JPEG. Results:

1. **Registration offset in side cells.** A sub-pixel ZNCC search slid a strip
   just left of the cut against the columns to its right.
   - 5.6K: **62.8 px mean** (62.4–63.4 over 8 cells, ZNCC 0.98–0.99), against
     62.55 predicted by PMOD.
   - Agreement score per model, on track-0 side cells across 4 frames:
     - PMOD model (S = 1313.45, 2O = 62.55): ZNCC 0.988–0.993 (left cell) and
       0.981–0.986 (right cell).
     - max2sphere (S = 1312, 2O = 64): 0.984–0.993 and 0.968–0.979.
     - Immuch360's no-squeeze 32 px model [12]: 0.866–0.944 and **0.469**–0.904.

     That model is **refuted**.
   - 3K: 30.3 px mean, against 32 for max2sphere's template. This is left open
     (§7).
   - Track-1 side cells (ground and sky) had too little texture for a reliable
     lag. They are covered by tests 2 and 3.
2. **Face table and orientations.**
   - Edges shared between a track-0 face and a track-1 face, across 4 frames,
     correlate as follows: LEFT|BACK 0.97–0.98, RIGHT|BACK 0.89–0.92,
     FRONT-top|UP 0.99–1.00, FRONT-bottom|DOWN 0.38–0.78 (a low-texture nadir).
   - A wrong-orientation control scores −0.17 to 0.27.
3. **End to end.**
   - An equirect rendered by the §2.5 sampler is visually seamless and upright.
   - Separately, I built a de-overlapped 3H×2H canvas (top row LEFT·FRONT·RIGHT,
     bottom row DOWN·BACK·UP, faces kept in their stored orientation) and ran
     FFmpeg `v360=eac:e` on it.
   - The two equirects match at **ZNCC 0.990** overall (0.974 in the
     equatorial band, 0.93 in the nadir band) with zero phase-correlation
     shift. The left/right-mirrored control scores 0.65.
   - So the GoPro strips are YouTube-EAC oriented once the overlap is removed,
     and both equirects put FRONT at the centre with +X to the right.
4. **Small anomaly.** In the RIGHT cell, the best integer match had a
   **1 px vertical offset** between the two registrations, in every frame
   at both resolutions (0 px in the LEFT cell). This is probably per-unit stitch
   calibration, and the crossfade hides it. Logged in §7.

### 2.9 Other implementations, compared

| Project | Model | Notes |
|---|---|---|
| max2sphere [3] (Apache-2.0) | Hard-coded templates `{4096,1344,1376,1344,32,5376}` and `{2272,736,768,736,16,2944}` (line 15). Effective S = side − 2·blend and 2O = 2·blend: (1312, 64) at 5.6K and (736, 32) at 3K. Linear blend over 2·blend px (`GetColour`, lines 450–540) | Same face table. It expresses DOWN/BACK/TOP through `RotateUV90` (u, v) → (v, 0.9999 − u) (line 560). Its `bitmaplib` JPEG reader stores images bottom-up, so mind the v flip when porting, as MGT-python notes [15]. Up to 1.5 px off at 5.6K |
| FFmpeg `v360=eac` [8] | YouTube 3×2 EAC: LEFT FRONT RIGHT / DOWN BACK UP, rotations 0,0,0 / 270,90,270, and an assumed **2-px padding** per face (`vf_v360.c` lines 2680–2693) | Cannot read a `.360` directly (two tracks, overlap bands). Works for previews after de-overlapping (§2.8). The 2-px inset is about 0.13° at H = 1344, and spirula-studio reports it as a visible step [13]. |
| goproMax-ffmpeg-v5 [9] (LGPL, FFmpeg fork) | OpenCL `gopromax` filter: `OVERLAP 64`, `CUT 688`, `BASESIZE 4096`, scaled proportionally to the input width (`libavfilter/opencl/gopromax.cl` lines 21–23, 212–213) | 5.6K MAX constants. Not PMOD-driven, so it is wrong for MAX 2 cells. |
| gopromax-conversion-tools [10] (MIT) | FFmpeg `crop`/`geq`/`scale` filtergraph (`scripts/360.avfilter`) | Since 2026-02, probes stream indices instead of using 0 and 5. |
| max2-reframe-resolve [11] (MIT) | MAX 2: `ovl = W − 3H`/2, `half = H/2 + (W−3H)/4`, no squeeze, linear blend over `ovl` (`proto/eac.py`, `src/reproject.cpp`) | Agrees with §2.4 when k = 1. The best open reference for MAX 2. |
| telemetry-parser [5] (MIT/Apache) | Emits face windows only (whole cells, `FaceOrientation`) plus `ProjectionKind::Eac {overlap, pre_scale, post_scale}` [6]. The consumer blends | Agrees with §2.2–§2.4. |
| Immuch360 [12] (AGPL-3.0) | Overlap `(W−3H)/2` with no squeeze, so 32 px offset on MAX. Also claims "the MAX needs a quarter turn about the front axis" | Offset refuted (§2.8). The table works on MAX frames without any turn [M]. The turn is probably the camera's roll in their clip, which is a gravity/CORI matter, not format. |
| VR180-Silver-Bullet [14] (MIT) | "5-tile row, centre 1920, `tile = (W−1920)/4`", and "original MAX = 5952" | **Wrong for MAX** (4096×1344). The 5696 width it reports for MAX 2 is hearsay. |
| spirula-studio [13] | MAX: `face = H`, `strip = (W−3H)/2 = 32`, cut out of each side cell's middle. **MAX 2: "each track is a whole equirect panorama"** | MAX: this is the same 32 px offset as Immuch360, refuted for the intra-cell registration (§2.8). Its own test minimised the seam *between tracks*, a different measurement. MAX 2: conflicts with everything else (§3). |

## 3. MAX 2 differences

The camera launched in 2025. GPMF support was added to gpmf-parser on
2025-10-15 (commit `be478ba`, "Added MAX2 metadata") [1].

| Item | Finding | Status |
|---|---|---|
| Container and tracks | Two HEVC video tracks, a stereo track and a 4-channel ambisonic track [11]. Native track order unknown | **[S]**, order **[U]** |
| Track size | 5952×1920 when 10-bit (P010), 5888×1920 when 8-bit [11][12]. telemetry-parser documents 2016/1920/2016 cells [5] | **[C]** for 5952, **[S]** for 5888 |
| Other sizes | One project reports a 5696-wide stream "seen in the wild" [14]. That width cannot be three cells around a 1920 face (`2·post + 1920 = 5696` gives post < H). Lower-resolution or high-frame-rate modes are undocumented | **[U]** |
| Projection | `PRJT = "EACO"`, as on the MAX [5][13]. Same face table [5][11] | **[C]** |
| PMOD | `[48, 2016, 2016, X]`: **no horizontal squeeze** (pre == post) and a **fourth value of unknown meaning** [5] (lines 62–63) [13] | **[C]** that a 4th value exists. Its meaning is **[U]** |
| Overlap | 2·48 = 96 px lag measured by telemetry-parser's author on MAX 2 pixels [5] (lines 118–121). For 8-bit files, 64 px is implied by [11] | **[S]** |
| Bit depth and log | 8-bit and 10-bit. A 10-bit GP-Log recording keeps 10 bits [11] (README lines 29–30). gpmf-parser lists `PTCL` values including `"LOG"` and `"GPLOG2"` [1] (README line 717). Which one MAX 2 writes is unknown | **[S]**, value **[U]** |
| Codec | HEVC. 10-bit is Main10 (`hvcC` profile 2 in [12]'s fixture) | **[S]** |
| New GPMF | `MAGN` (raw Hall sensor, 24 Hz, µT), `MNOR` (fused magnetic-north vector, per frame), `OCNF` (MNOR confidence 0–255), `CSCT`/`CSCB` ("Compression Score Top/Bottom (EAC Slice)", GoPro internal). Otherwise "supports all HERO13 metadata" [1] (README lines 641–649) | **[S]**, from GoPro itself |
| Audio | AMB is `pcm_s24le`, 4 ch, 48 kHz, AmbiX W, Y, Z, X, stream index 5 [14] | **[U]** |
| Frame rates | Not documented in any source found. max2-reframe-resolve falls back to 30000/1001 when it cannot read them [11] (`src/source360.cpp` line 197) | **[U]** |

**The conflict.** spirula-studio [13] (`docs/datasets.md` lines 658–690)
claims each MAX 2 track is a whole equirect panorama, `1008 + 3936 + 1008`,
and that the strongest column discontinuities are at x = 1008 and x = 4944.
Those two columns are exactly the lens cuts of the EAC model: the centre lines
of the 2016-wide side cells (1008 = 2016/2 and 4944 = 2016 + 1920 + 1008).
Four independent sources support EAC:

- GoPro's own "EAC Slice" tag names [1].
- telemetry-parser's pixel measurement [5].
- max2-reframe-resolve's fit to GoPro Player exports [11].
- Immuch360 [12].

I rate the EAC model **[C]** and the panorama claim a misreading. A MAX 2
sample (§7) closes it.

## 4. GPMF telemetry

### 4.1 Format basics

These are from gpmf-parser [1] (README and `GPMF_common.h` lines 67 and
111–125). **[C]**

- **KLV structure.** A 4-byte FourCC, a 1-byte type, a 1-byte struct size and
  a 2-byte big-endian repeat count. Payloads are padded to 4 bytes and all
  values are big-endian.
- **Nesting.** Type 0 means nested KLVs:
  `DEVC > STRM > {STMP, TSMP, STNM, SIUN/UNIT, SCAL, TYPE, ORIN, ORIO, MTRX, TMPC, <data>}`.
- **`SCAL`** is a **divisor**. It is either one value for all components or
  one value per component.
- **`TSMP`** is the running total of samples, including the current payload.
  **`STMP`** is the time stamp of the payload's first sample, in µs.
- **`TICK`/`TOCK`/`TIMO`** are rare timing fields.
- **Type `'#'`** means a Huffman-compressed payload. `DISP` uses it [2].

### 4.2 Streams in a real MAX file **[M]**

This is gpmf-parser's `samples/max-360mode.mp4`: MAX, firmware
H19.03.01.30.00, 23.976 fps, 12 payloads of 1.001 s. The per-payload count is
TSMP growth.

| FourCC | STNM | Type | SCAL | Units | Per payload | Rate on the STMP clock |
|---|---|---|---|---|---|---|
| GYRO | Gyroscope | `s`×3 | 939 | rad/s | 789–790 | 789.83 Hz |
| ACCL | Accelerometer | `s`×3 | 417 | m/s² | 197–198 | 197.46 Hz |
| MAGN | Magnetometer | `s`×4 (8 bytes) | 1 | µT | 24–25 | 24.68 Hz |
| CORI | CameraOrientation | `s`×4 | 32767 | quaternion | 24 | 24.000 Hz |
| IORI | ImageOrientation | `s`×4 | 32767 | quaternion | 24 | 24.000 Hz |
| GRAV | Gravity Vector | `s`×3 | 32767 | unit vector | 24 | 24.000 Hz |
| SHUT | Exposure time | `f` | none | s | 24 | 24.000 Hz |
| ISOE | Sensor ISO | `S` | none | none | 24 | 24.000 Hz |
| WBAL, WRGB, UNIF | WB Kelvin, WB gains, image uniformity | `S`, `f`×3, `f` | none | none | 8 | 8.000 Hz |
| DISP | DisparityMatrix | `'#'` | none | none | 24 (compressed) | per frame |
| GPS5 (+GPSF, GPSU, GPSP) | GPS | `l`×5 | 1e7, 1e7, 1000, 1000, 100 | deg, deg, m, m/s, m/s | 17–19 | 18.18 Hz |
| VPTS | in the CameraOrientation STRM | `J` (u64) | none | µs? | 1 | +1 000 000 per payload |

- ACCL and GYRO carry `ORIN = "XzY"`, `ORIO = "ZXY"` and
  `MTRX = [0,−1,0; 1,0,0; 0,0,1]`, plus `TMPC` (sensor temperature, 30.5 °C).
- gpmf-parser documents MAX CORI, IORI and GRAV at "frame rate", DISP as a
  "1-D depth map" and MAGN at 24 Hz [1] (README lines 594–602). **[C]**
- MAGN has **four** int16 components, for example (−382, 7, 220, 6865). gpmf-parser
  says x, y, z, so the fourth is **[U]**.
- First CORI sample: (32682, 501, 2298, −4) / 32767, a non-identity unit
  quaternion. First IORI sample: (32767, 0, 0, 0), the identity.
  VR180-Silver-Bullet reports the same pattern on another MAX clip [14]
  (fixtures README). **[C]**
- First GRAV sample: (0.003, 0.9997, −0.025). The camera was upright
  (`OREN = "U"`) and gravity lies along GRAV's Y axis. **[M]**

### 4.3 Axis order (ORIN/ORIO/MTRX)

- `ORIN` is the "input channel data orientation, lowercase is negative".
  `ORIO` is the output orientation, and `MTRX` the matrix between them [1]
  (`GPMF_common.h` lines 111–113).
- The MAX writes its raw IMU channels as (X, −Z, Y). Applying MTRX (row-major,
  `out = M·in`) gives GoPro's standard **Z, X, Y** order, the one HERO cameras
  use [1] (README lines 531–532). **[M]** by arithmetic.
- telemetry-parser builds MTRX from ORIN/ORIO when MTRX is absent, turns it
  into an orientation string, and normalises IMU vectors with it [5]
  (`src/gopro/mod.rs` lines 323–345, 507–525).
- The mapping from GoPro's IMU axes to the EAC frame of §2.3 is **not
  documented** **[U]**. max2-reframe-resolve fitted it for CORI/IORI as
  `M = diag(−1, 1, −1)`, a 180° turn about Y [11] (`src/source360.cpp`
  line 150). Pin it with sample #10.

### 4.4 Timing

- **Master clock.** gpmf-parser treats the MP4 as the master clock
  [1] (README "GPMF Timing and Clocks"). Its `GetGPMFSampleRate` takes rates
  from `TSMP` deltas over the `STMP` span, using MP4 payload times only to
  determine the STMP unit [1] (`GPMF_utils.c` lines 31–200).
- **The STMP clock is not the MP4 clock.** In the 23.976 fps sample, every
  per-frame stream advances exactly 24 samples per 1 000 000 µs of STMP (24.000
  Hz), while each payload lasts 1.001 s of MP4 time. **[M]**
  - So `MP4 time ≈ 1.001 · STMP`.
  - Placing IMU samples on raw STMP drifts 0.1 % against the video, which is
    0.6 s over 10 minutes.
  - VPTS also advances exactly 1 000 000 per payload, at a constant offset of
    +1 083 334 from CORI's STMP. **[M]** Semantics **[U]**.
- **Coverage.** The sample has 291 video frames but 280 samples in each
  per-frame stream, and the first per-frame STMP is 176 ms. Per-frame metadata
  does not cover every frame, so it cannot be aligned by index from frame 0.
  **[M]**
  - Tools disagree here. max2-reframe-resolve maps frame i to metadata index
    `i·m/n` [11] (`metaIndex`). telemetry-parser times CORI from the first
    STMP [5] (`mod.rs` lines 385–438).
  - **[U]**: needs sample #10.
- **Recommended for Spherecut:**
  1. Fit `t_video = α·STMP + β` by least squares over all payloads. Pair each
     payload's first STMP for SHUT (or CORI) with that payload's MP4 start
     time from the `gpmd` `stts`.
  2. Place every sample with it: sample j of a payload is at
     `STMP + j·(next STMP − STMP)/n`.
  3. Validate α ≈ 1.001 (for 1000/1001 frame rates) and β against sample #10.
- **telemetry-parser.**
  - `util::normalized_imu` re-times GoPro IMU samples as
    `i · avg_duration`, where the duration comes from the STMP span
    (`GoPro::get_avg_sample_duration`). That puts them on the STMP clock, and it
    writes ACCL samples into GYRO slots by index even though ACCL runs at a
    quarter of the rate [5] (`src/util.rs` lines 304–419).
  - **Use `util::normalized_imu_interpolated`.** It keeps per-sensor rates and
    start offsets (`GoPro::imu_start_times_ms`) [5] (`src/util.rs` lines
    421ff, `mod.rs` lines 445–457). Then apply the α correction above.
    **[S]**, by code reading.

### 4.5 CORI, IORI and GRAV

- gpmf-parser: CORI is "camera orientation since capture start", IORI is
  "image orientation relative to the camera body", and GRAV is the gravity
  direction [1] (README lines 588–590).
- telemetry-parser divides by SCAL, reads (w, x, y, z) with **x negated**, and
  publishes `CORI * IORI` as `GroupId::Quaternion` [5] (`mod.rs` lines
  395–436).
- max2-reframe-resolve uses `T = M · R(IORI) · R(CORI) · Mᵀ` with
  `M = diag(−1, 1, −1)`. It levels the horizon with `Tᵀ · (M · GRAV)`, fitted
  to GoPro Player exports [11] (`src/source360.cpp` lines 215–232).
- These conventions differ. Treat both as hypotheses until sample #10 pins
  sign, order and frame.
- **Open:** is IORI already baked into the `.360` pixels when in-camera
  stabilisation or horizon levelling is on? The header has `EISE = "Y"` and
  `EISA = "HS EIS"` [2]. **[U]**, needs sample #11.

### 4.6 Header (`udta`) fields worth reading

From [2], plus definitions in [1] (README lines 690–740). **[M][S]**

| Field | Example | Use |
|---|---|---|
| `MINF` | `GoPro Max` | Model |
| `FMWR` | `H19.03.01.30.00` | Firmware |
| `PRJT` | `EACO` (.360), `GPRO` (HERO mode), `FSFB` (low-res dual fisheye) | Projection. telemetry-parser's enum: ERP0, EAC0, EACT/EACB, **EACO**, PEAC/PEAT/PEAB, FSHF/FSHB, **FSFB**/FSBF, PANO/TINY [5] (`eac.rs` lines 31–44) |
| `PMOD` | see §2.2 | EAC geometry. Only on EACO files [5]. Absent from the FSFB sample [M] |
| `PTCL` | `FLAT` | Protune colour (`LOG`, `GPLOG2` on newer cameras) |
| `EISE`, `EISA` | `Y`, `HS EIS` | Stabilisation state |
| `OREN` | `U` | Mounting orientation |
| `SROT` | 20.89 ms | Sensor read-out |
| `MUID` | 8×u32 | Media ID. Possibly shared with the LRV (§5) **[U]** |
| DEVC `Geometry Calibrations` | `SHFX/Y/Z`, `ANGX/Y/Z` (ANGY ≈ 180.04°), `CALW×CALH` = 4056×3040 | Lens-to-lens extrinsics |
| DEVC `Back Lens`, `Front Lens` | `KLNS` (5 doubles), `CTRX`, `CTRY`, `MFOV` (100), `SFTR` | Per-lens intrinsics. Needed only for fisheye (LRV) or re-stitching. Semantics **[U]** |

### 4.7 What telemetry-parser gives you

These come from [5] at commit 2817b42, `Cargo.toml` version 0.4.0.

- `Input::from_stream(&mut reader, size, path, progress_cb, cancel_flag)`
  detects GoPro by content. Extensions `mp4`, `mov`, `360`, `gpmf` and `lrv`
  are accepted (`src/gopro/mod.rs` line 58).
- Per-payload `SampleInfo { timestamp_ms, duration_ms, tag_map }`, with groups
  `Gyroscope`, `Accelerometer`, `Magnetometer`, `CameraOrientation`,
  `ImageOrientation`, `Quaternion` (CORI·IORI), GPS and exposure, among others.
- `util::normalized_imu{,_interpolated}` returns `IMUData { timestamp_ms, gyro
  (deg/s), accl (m/s²), magn }`, already reoriented (`src/util.rs` lines
  290–296).
- `util::camera_rig(&input)` returns a `CameraRig` with
  `ProjectionKind::Eac { faces, overlap, pre_scale, post_scale }`. It has six
  `CubeFace`s (whole-cell rectangles plus orientation) and
  `pictures = [VideoTrack(0), VideoTrack(1)]`. This happens only for
  `PRJT = "EACO"` files that pass the PMOD identities (`eac.rs` lines 55–80).
  The types are from the `video-types` crate [6].

## 5. The `.LRV` companion

| Item | Finding | Status |
|---|---|---|
| Video | 1408×704 H.264 High, `yuvj420p` full range, BT.709, about 2.5 Mb/s, at the clip's frame rate (25 fps in [17], 23.976 in [2]) | **[M][C]** |
| Other tracks | AAC stereo, `tmcd`, GPMF ("GoPro MET"), "GoPro SOS" [2][17] | **[M][C]** |
| Layout | **Dual fisheye side by side, not EAC or equirect.** Header `PRJT = "FSFB"` [2]. A frame shows two circles | **[M]** |
| Which lens is left | `FSFB` suggests front first and `FSBF` would be back first [5] (`eac.rs` line 42). MGT-python treats the LRV as front then back, with a v flip relative to max2sphere [15] | **[S][U]** |
| Fisheye FOV and model | Calibration in the `udta` (`KLNS`, `CTRX/CTRY`, `MFOV = 100`, GEOC extrinsics) [2]. One project emulates the LRV with 190° circles at yaw ±90° [16], but that is an emulation and was not measured | **[U]** |
| Pairing | Same directory and same `ccnnnn` digits as the `.360`. The exact LRV prefix on MAX is not verified: HERO cameras use `GL…LRV`, and on the MAX `GL` also names HERO-mode videos [19]. Pair by digits, duration and timecode, and compare `MUID` if the LRV shares it | **[U]** |
| Frame sync | Same timecode track. `LSKP` ("Low res video frame SKiP ... improves sync with the main video when using the LRV as a proxy") exists on HERO9+ [1]. Whether MAX or MAX 2 write it is unknown | **[U]** |
| MAX 2 LRV | No information found | **[U]** |

- gpmf-parser's `samples/max-360mode.mp4` has exactly these properties: one
  AVC track at 1408×704 and `PRJT FSFB`. It is probably an LRV, renamed or
  written in a low-resolution mode **[U]**.
- telemetry-parser reads LRV telemetry but builds no rig for FSFB. It refuses
  every non-EACO projection by name [5] (`eac.rs` lines 45–61).
- **For Spherecut:** the LRV is usable as a proxy only with a separate
  dual-fisheye shader path whose calibration is not yet understood.
  - Simpler option: build proxies from the `.360` by decoding at reduced
    resolution, or by transcoding to a small EAC or equirect proxy.
  - Use the LRV for thumbnails and scrubbing first.

## 6. Licences, crates and GPL-3.0-or-later

| Component | Licence | Use under GPL-3.0-or-later | Notes |
|---|---|---|---|
| gopro/gpmf-parser [1] (C) | **Apache-2.0 OR MIT** (README lines 30–31, `LICENSE-APACHE`, `LICENSE-MIT`) | Compatible. Choose MIT, or Apache-2.0, which is GPLv3-compatible | Reference for KLV, timing and tag tables. A Rust port of the parts we need is small |
| trek-view/max2sphere [3] (C) | **Apache-2.0** (README lines 185–187, `LICENSE`) | Compatible with GPLv3 (not with GPLv2-only). Keep notices if code is copied | Better to reimplement from §2 (PMOD-driven) than to copy |
| AdrianEddy/telemetry-parser [5] (Rust) | **MIT OR Apache-2.0** (SPDX headers, `Cargo.toml`) | Compatible | **crates.io has only 0.2.6 (2022-09-06)**, which predates the `.360` rig. Git master is `version = "0.4.0"` (commit 2817b42, 2026-10-22) [5][7] |
| ├ `mp4parse-patched` 0.17.1 | MPL-2.0 [7] | Compatible (MPL-2.0 §3.3), unless a file is marked "Incompatible With Secondary Licenses". Not checked **[U]** | Already in Spherecut's `deny.toml` allow-list |
| ├ `video-types` 0.1.2 | MIT OR Apache-2.0 [6][7] | Compatible | `CameraRig`/`ProjectionKind` types |
| ├ `fc-blackbox-patched` 0.2.1 | MIT OR Apache-2.0 [7] | Compatible | Betaflight log parser, unused for GoPro |
| └ others | `prost`, `serde_yaml` 0.9, `argh`, `csv`, `chrono`, … [5] (`Cargo.toml`) | Permissive | `serde_yaml` is archived upstream. cargo-deny may flag it as unmaintained **[U]** |
| `gpmf` crate 0.1.2 (apps4uco) | MIT OR Apache-2.0 [7] | Compatible | Alternative GPMF parser, not evaluated |
| FFmpeg [8] | LGPL-2.1-or-later (GPL with `--enable-gpl`) | Compatible | `v360` for previews and tests only |
| max2-reframe-resolve [11], gopromax-conversion-tools [10], VR180-Silver-Bullet [14] | MIT | Compatible | References. [14] has known-wrong EAC dimensions |
| goproMax-ffmpeg-v5 [9] | LGPL (FFmpeg fork) | Compatible | 5.6K constants only |
| Immuch360 [12] | **AGPL-3.0** | Do **not** copy code | Its overlap model is refuted anyway |
| spirula-studio [13] | not checked | Do not copy | Cited only for its claims |

Practical consequences for Spherecut:

- `deny.toml` sets `unknown-git = "deny"`, so a git dependency on
  telemetry-parser needs an `allow-git` entry and a DECISIONS.md record.
- The alternatives are vendoring the MIT/Apache GoPro modules, or writing a
  small GPMF reader. The reader needs:
  - about 15 tags;
  - `udta` `PRJT`/`PMOD`/`MINF`;
  - the KLV walker and `'#'` decompression, which is only needed for DISP.
- Either way, keep telemetry-parser as a test oracle.

## 7. Open questions (each needs real sample files)

The sample numbers refer to `docs/SAMPLES.md`.

1. **The `udta` of a real `.360`.** Dump `PRJT`/`PMOD`/`MINF` from MAX 5.6K,
   MAX 3K and both MAX 2 bit depths. This confirms [32, 1408, 1376] and
   [48, 2016, 2016, X], gives the **3K values** (measured lag 30.3 vs 32), and
   the **meaning of `PMOD[3]`** on MAX 2.
2. **MAX 2 layout.** Confirm EAC rather than spirula's equirect claim: render
   with §2.5 and check seams. Then measure the 8-bit 5888 overlap (64?),
   check for other widths (5696?), and record the modes' sizes and frame
   rates (samples #1–#5).
3. **MAX 2 native track order**, handler names and AMB codec (s24 or s32), so
   that track selection by handler is proven on it.
4. **Ambisonics** (sample #6): whether the camera-original file has an SA3D
   box. Confirm W, Y, Z, X and SN3D, and that +X/+Y of the sound field match
   FRONT/LEFT of §2.3 by walking around the camera while talking.
5. **Both video tracks' `stts`/`stss`.** Are they identical? Are GOPs aligned?
   Are there B-frames? This decides how simple lockstep seeking can be.
6. **Telemetry-to-video alignment** (sample #10). The STMP-to-MP4 slope
   (1.001?) and offset. Which frame CORI/IORI sample 0 belongs to (the first
   STMP was 176 ms, and there were 280 samples for 291 frames). VPTS
   semantics.
7. **IMU and orientation axes** (sample #10): IMU frame to EAC frame, and the
   sign, order and frame of the CORI/IORI quaternions. Which composition is
   right: CORI·IORI as in telemetry-parser, or M·I·Q·Mᵀ as in max2-reframe-resolve?
8. **Stabilisation baked in?** (sample #11). Do EAC pixels move with IORI when
   HyperSmooth or horizon levelling is on? Is IORI identity when it is off?
9. **The 1-px vertical offset** between registrations in the RIGHT cell: is it
   per unit or per format?
10. **`.LRV`**: confirm the FSFB layout on a fresh LRV, the lens order, the
    fisheye model and FOV (KLNS semantics), the file-name prefix, `MUID`
    sharing, and `LSKP`. Does MAX 2 write an LRV at all, and in what layout?
11. **`DISP`** (Huffman-compressed per-frame disparity): decode it and see
    whether it helps parallax-aware seam blending. Low priority.
12. **MAGN's fourth component** and MAX 2's `MNOR`/`OCNF` frames.
13. **Chapters**: is the timecode and telemetry continuous across `GS01…` and
    `GS02…` (sample #13)?
14. **Spherical metadata**: confirm that a camera-original `.360` has no
    `sv3d`/`st3d`, and check what GoPro Player's equirect export carries
    (sample #12, which is also the visual reference for seam blending).

## Sources

Commits are the ones read on 2026-10-22.

1. gopro/gpmf-parser, commit `9a71506` (2026-08-18). <https://github.com/gopro/gpmf-parser>
   - `README.md`: MP4 structure, timing, the MAX table (lines 594–602), the MAX2 table (lines 641–649), the header table (lines 690–740), licence (lines 30–31).
   - `GPMF_common.h` lines 67 and 111–125.
   - `GPMF_utils.c` `GetGPMFSampleRate` (lines 31–200).
   - Commit `be478ba` (2025-10-15), "Added MAX2 metadata".
2. gopro/gpmf-parser `samples/max-360mode.mp4` and `samples/max-heromode.mp4`: real MAX files, firmware H19.03.01.30.00. FFprobe output, the `udta` GPMF and the `gpmd` track were parsed for this note.
3. trek-view/max2sphere, commit `bb6524a` (2023-08-15). <https://github.com/trek-view/max2sphere>
   - `max2sphere.c`: line 15 (templates), lines 336–390 (`FindFaceUV`), lines 450–540 (`GetColour`), line 560 (`RotateUV90`), lines 586ff (face planes).
   - `README.md` lines 70–76 and 185–187.
   - `testframes/5_6k`, `testframes/3k`: real MAX frames plus GoPro Player's `STITCHED` equirect.
4. Paul Bourke, "Converting GoPro MAX 360 files", <http://paulbourke.net/panorama/gopromax2sphere/> (read via WebFetch).
5. AdrianEddy/telemetry-parser, commit `2817b42` (2026-10-22), `Cargo.toml` version 0.4.0. <https://github.com/AdrianEddy/telemetry-parser>
   - `src/gopro/eac.rs` (whole file).
   - `src/gopro/mod.rs`: line 58, lines 323–345, 385–438, 445–505.
   - `src/gopro/udta.rs`, `src/gopro/klv.rs`.
   - `src/util.rs`: lines 290–420, 421ff.
   - `src/lib.rs` line 71.
   - Commits "Add support for parsing mcraw and 360 cameras" (2026-10-08) and "Add rig kind to 360 metadata" (`e4eb02e`, 2026-10-19).
6. `video-types` 0.1.2 (crates.io; repository AdrianEddy/wgpu-interop): `src/geometry.rs` (`Face`, `FaceOrientation`, `ProjectionKind::Eac`).
7. crates.io API, queried 2026-10-22.
   - `telemetry-parser`: max 0.2.6 (2022-09-06).
   - `video-types`: 0.1.2.
   - `mp4parse-patched`: 0.17.1, MPL-2.0.
   - `fc-blackbox-patched`: 0.2.1.
   - `gpmf`: 0.1.2.
   - `mp4parse`: 0.17.0, MPL-2.0.
8. FFmpeg/FFmpeg master `179d083` (2026-10-22).
   - `libavfilter/vf_v360.c`: lines 2617–2666 (`prepare_eac_in`/`_out`), lines 2676–2777 (`eac_to_xyz`, 2-px padding at lines 2680–2693), line 2779 (`xyz_to_eac`).
   - `libavformat/mov.c` lines 9303–9385 (`mov_read_SA3D`).
9. gmat/goproMax-ffmpeg-v5, commit `26ca210` (2025-07-03): `libavfilter/opencl/gopromax.cl` lines 21–26 and 212–279, `libavfilter/vf_gopromax_opencl.c`, `doc/filters.texi`.
10. Jusas/gopromax-conversion-tools, commit `0072283` (2026-02-01): `scripts/360.avfilter`, `scripts/360.avfilter.readme`. MIT.
11. Sean-McConnachie/max2-reframe-resolve, commit `9170bca` (2026-10-15). MIT.
    - `README.md` lines 29–30, 95 and 198–199.
    - `proto/eac.py` lines 1–50, `proto/stab.py`.
    - `src/reproject.cpp` lines 13–16.
    - `src/source360.cpp` lines 60–85, 150, 168–212, 215–232.
    - `src/reframe_kernel.h` line 148.
    - `src/mp4.cpp` lines 80–82.
12. freeKC/Immuch360, commit `3f58875` (2026-10-22). AGPL-3.0.
    - `mobile/lib/domain/services/raw/gopro_eac.dart` lines 1–60.
    - `mobile/test/fixtures/raw/gopro.stub.dart`.
13. harry7557558/spirula-studio, commit `9a24f09`: `src/app/Pano360.h`, `docs/datasets.md` lines 615–700.
14. silverqsy/VR180-Silver-Bullet, commit `e269252` (2026-10-17). MIT.
    - `crates/vr180-core/src/eac/dims.rs`.
    - `crates/vr180-pipeline/src/audio.rs` lines 1–70.
    - `crates/vr180-core/tests/fixtures/README.md`.
15. fourMs/MGT-python, commit `baf00c2`: `musicalgestures/_remap360.py`.
16. alexarje/GoPro-360-to-dual-fisheye, commit `6c40c6e`: `docs/TECHNICAL.md`.
17. alexarje/stillstanding, commit `64ea64d`: `code/processed.ipynb`. FFmpeg output for camera-original MAX `.360` files (stream map 0:0, 0:5, 0:1, 0:6) and `.LRV` files.
18. JhnsonO/ffa-automations, commit `e29590f`: `logs/uploader.log` lines 100–138. FFmpeg listing of a GoPro-cloud `GS010404.360`, MAX firmware H19.03.02.06.00.
19. trek-view/gopro-metadata, commit `f624f73`: `max/README.md` (file naming, chapters, LRV/THM).
20. GoPro, "MAX tech specs: stitching, resolution", <https://gopro.com/en/se/news/max-tech-specs-stitching-resolution> (read via WebFetch).
21. exiftool/exiftool master, `lib/Image/ExifTool/GoPro.pm`: consulted. It has no `PMOD` entry.
