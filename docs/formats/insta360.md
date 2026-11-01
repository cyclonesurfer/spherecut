# Insta360 raw video: `.insv` and `.lrv`

Status: desk research, 2026-10-22, then checked against Pat's own X5 samples on 2026-10-23,
with corrections from a second look at their bytes on 2026-11-01 (M3.0) and a third, of the
trailer's gaps, the metadata record and the four strings, the same day (M3.2)
(see [Pat's X5 samples](#pats-x5-samples), which take precedence over the rest where they
disagree). Before the samples, the only real camera bytes behind this document were the
Insta360 X5 slices that GyroView publishes as test fixtures (two recordings: an info record,
the trailer footer and index, and the first gyro and exposure samples). I decoded them myself
with throwaway scripts. Everything else comes from other projects' source code and notes, or
from Insta360's public manuals. Each source is pinned to a commit in [Sources](#sources).

Spherecut must not use Insta360's Media SDK, Camera SDK or Insta360 Studio, and must not copy
code or data tables derived from them. Section 8 covers which references are clean.

## Pat's X5 samples

Seven recordings from Pat's **Insta360 X5, firmware `v1.1.22_build1`** (field 3), made
2026-10-22, examined with `spherecut probe` and a throwaway trailer script. File numbers are
the `_004` to `_010` suffixes. Pat shot them in the order of `docs/SAMPLES.md` and skipped
clips 5 to 7, which matches the modes and lengths below.

| File | Mode (from the trailer) | Tracks | Length | Clip |
|---|---|---|---|---|
| 004 | 8K30: 2 × 3840² HEVC, 29.97 fps | 2 video + AAC | 3.8 s | 1 |
| 005 | 8K25: 2 × 3840², 25 fps | 2 video + AAC | 4.6 s | 2 |
| 006 | 5.7K60 HDR video: 2 × 2880², 59.94 fps | 2 video + AAC | 3.2 s | 3 |
| 007 | 4K120: 2 × 1920², 119.88 fps | 2 video + AAC | 4.9 s | 4 |
| 008 | 8K30 | 2 video + AAC | 32.2 s | 8: tripod, lines across the seams |
| 009 | 8K30 | 2 video + AAC | 63.5 s | 9: handheld, fast turns, two flips |
| 010 | 8K30 | 2 video + AAC | 32.3 s | 10: slow yaw, pitch and roll on the tripod |

What they show [bytes]:

- **One file per take in every mode**, with both lenses as two `hvc1` tracks (handler
  `INS.HVC`), plus an `LRV_…_01_….lrv` proxy. No `_10_` files.
- **Box order** `ftyp` `wide` `mdat` `moov` `inst`. So the `wide` box is there (orbis was
  right), and the `inst` wrapper is already used by firmware v1.1.22, which contradicts
  GyroView's "from v1.7 on" (§2.1). `inst.size` = trailer size + 8 on all 14 files.
- **Video**: HEVC Main, 8-bit `yuvj420p`, full range, BT.709 primaries, transfer and matrix,
  chroma location left, level 6.1 (8K) or 5.1 (5.7K, 4K). About 62 Mb/s per track at 8K30,
  79 Mb/s at 5.7K60. Field 135 (bitrate) says 120 Mb/s, or 150 at 5.7K60, for the two tracks
  together.
- **Timebases**: 1/30000 at 29.97, 1/25000 at 25, 1/60000 at 59.94, 1/120000 at 119.88, with
  `avg_frame_rate` exactly 30000/1001, 25/1, 60000/1001 and 120000/1001.
- **Both tracks have the same frame count** in every file.
- **LRV**: H.264 High, 1664×832, about 8 Mb/s, full-range BT.709. It runs at **25 fps for a
  25 fps master** and 29.97 fps otherwise, so "always 29.97" (§6) is wrong. It has exactly the
  master's frame count at 25 and 29.97 fps, half at 59.94 (95 of 190) and a quarter rounded up
  at 119.88 (147 of 586). Its trailer has the master's records, and **every record but the
  metadata (1) is byte-identical to the master's** on all 7 pairs, the IMU, exposure, 22 and
  28 included (SHA-256 of each payload, 2026-11-01). Its metadata is the master's, field for
  field and byte for byte, the four calibration strings included, but for these, on all 7 pairs
  [bytes] (M3.2): 9 (the trailer's start, §3.1), 26's index (its item 2) = 1, 36 (a Unix time in
  ms, under a second from the master's), 44 (1664 × 832, the LRV's own video), 79 and 80
  (absent), 98's item 5 (the LRV's own frame count), 131 = 1, 132 = 1 (H.264) and 135 = 7.
- **LRV halves**: the left half is master **track 1** and the right half **track 0**. This is
  shown by the bytes, not inferred: on frame 60 of 004 and of 008, each half's luma correlates
  0.986 to 0.992 with its track scaled to 832², against 0.40 to 0.49 with the other track
  (2026-11-01; M3's plan, 3.1, has 0.985 to 0.995 against 0.41 to 0.50). Which track is the
  screen-side lens is a separate question (§2.6, Q2).
- **Audio**: one AAC-LC track, 48 kHz, 2 channels, in every file. Field 61 `audio_mode` = 9
  everywhere. No 360-audio recording yet, so Q15 stays open.
- **Trailer**: version 3, a 31-slot index, records 1, 2, 3, 4, 9, 10, 11, 22, 28 and 29.
  Records are 128 KiB aligned except 1 and 10, as in §3.2. Record 22 is 8 306 836 bytes and
  record 28 12 793 bytes in every file, but **their bytes differ in every take**: 7 distinct
  SHA-256s each over the 7 takes, while each LRV's equal its master's (2026-11-01). So they are
  not fixed per-camera data, as the 2026-10-23 reading of their sizes alone suggested. The IMU
  record (3) grows by about 20 kB per second.
- **Trailer layout** [bytes] (M3.2): in every file the records lie in the order 29, 28, 22, 11,
  10, 9, 4, 3, 2 (absent from the 4K120 pair), 1, then the index, and record 1's footer ends
  where the index starts, at EOF − 388, not at the index's footer (EOF − 78). The gaps between
  records are **not zero padding**: every one of the 14 files has non-zero bytes in at least one
  gap between two records, mostly in the gaps before records 9, 4 and 2. Counted from one
  record's footer to the next record's start (a blank is a gap of zeros):

  | File | before 22 | before 11 | before 9 | before 4 | before 2 |
  |---|---|---|---|---|---|
  | VID 004 | 1 | | | | |
  | VID 005 | 1 | | | | 57 |
  | VID 006 | | | 17 | | 186 |
  | VID 007 | 1 | 35 | 176 | 64 | (no record 2) |
  | VID 008 | | | 184 | 127 | 158 |
  | VID 009 | | | 133 | 162 | 205 |
  | VID 010 | | | 239 | | |
  | LRV 004 | | | | 86 | 40 |
  | LRV 005 | | | | | 9 |
  | LRV 006 | | | 86 | 34 | 48 |
  | LRV 007 | | 159 | 56 | | (no record 2) |
  | LRV 008 | | | 42 | 127 | 114 |
  | LRV 009 | | | 30 | 162 | 162 |
  | LRV 010 | | | 149 | | 20 |

  The gaps are up to 256 KiB long, so these are stray bytes in mostly zero gaps (the most, 239,
  in a gap of 3 374 bytes). The gap from the payload start to record 29 has non-zero bytes too
  in 5 files (VID 004, 008, 010, LRV 004, 007). A reader finds every record through the index
  and never reads the gaps.
- **Thumbnail**: record 2 is 1 228 840 bytes (= 40 + 1280 × 640 × 1.5, consistent with the
  NV12 1280×640 equirect of §3.3). The **4K120 file has no record 2**; the camera wrote it as a
  sidecar `VID_….insv.thumbnail.bin` of the same size instead, and the same for its LRV. The
  5.7K60 take has a `….insv.trim_thumbnail.bin` sidecar. Spherecut ignores these sidecars.
- **Metadata**: `gamma_mode` `standard`, `is_flowstate_online` 0, `cam_posture` 0,
  `is_raw_gyro` 1, `pts_type` 2, `capture_offset_version` 4 (V6), `image category` 2,
  `stream_type` 4, `codec` 2 (H.265) on every `.insv`. Field 26 `file_group_info.type` is 0,
  except **6 on the 5.7K60 take**, which telemetry-parser's enum calls `VideoHdr`. Pat confirms
  that take was shot in the camera's HDR video mode, so the enum value holds on the X5. The
  HDR take is still 8-bit BT.709 `standard`; HDR here is the camera's exposure mode, not an HDR
  transfer function.
- **Record 1's fields** [bytes] (M3.2): every master has **92** top-level fields, numbered up to
  163, none of them twice; every LRV has the same but **79 and 80**, so 90. Spherecut's reader
  leaves 67 of them unread on every file. Field 133 is absent and 134 is 0 everywhere. Field 25
  is an **f32 widened to a double**: 21.244001388549805 at 8K, 9.181875228881836 at 5.7K60 and
  5.53125 at 4K120, so it compares by its bits, not as 21.244. Field 44 is **the file's own
  video size**: one track's on a master (3840², 2880² at 5.7K, 1920² at 4K), 1664 × 832 on an
  LRV. Fields 79 = 2 and 80 = 1 on the masters.
- **Fields 93 and 98 aren't the clip's length and frame count** [bytes] (M3.2). With *n* one
  more than the frames in each of the master's tracks, *num/den* the master's rate and *r* that
  rate rounded up to a whole number (30, 25, 60, 120), on the master and on its LRV alike:
  field 93 = ⌊*n* · 1000 · *den* / *num*⌋; 98's item 1 = field 24; item 4 − item 3 =
  ⌊*n* · 1000 / *r*⌋ ms and item 2 − item 1 = ⌊*n* · 10⁶ / *r*⌋ µs; items 6 and 7 are 0.
  Item 5 is *n* on a master and the LRV's own frame count on an LRV. So on 004 item 4 is
  3833 ms after item 3, where the last of its 114 frames starts 3770 ms in, and LRV 006 gives
  93 = 3186 from the master's 190 frames at 59.94, not 3203 from its own 95 at 29.97.

  | Take | Mode | Frames a track | 98.5, master / LRV | 93 | 98.4 − 98.3 (ms) | 98.2 − 98.1 (µs) |
  |---|---|---|---|---|---|---|
  | 004 | 8K30 | 114 | 115 / 114 | 3837 | 3833 | 3 833 333 |
  | 005 | 8K25 | 116 | 117 / 116 | 4680 | 4680 | 4 680 000 |
  | 006 | 5.7K60 | 190 | 191 / 95 | 3186 | 3183 | 3 183 333 |
  | 007 | 4K120 | 586 | 587 / 147 | 4896 | 4891 | 4 891 666 |
  | 008 | 8K30 | 966 | 967 / 966 | 32265 | 32233 | 32 233 333 |
  | 009 | 8K30 | 1904 | 1905 / 1904 | 63563 | 63500 | 63 500 000 |
  | 010 | 8K30 | 968 | 969 / 968 | 32332 | 32300 | 32 300 000 |
- **Record 4 and the IMU against the frames** [bytes] (M3.2). Record 4 holds an item a frame
  (§3.4), each a frame after the last to within 0.23 ms, from a few frames before the tracks'
  first to a few past their last, as many as the rate gives: field 24 is exactly the stamp of
  item 5 at 25 and 29.97 fps, item 6 at 59.94 and item 10 at 119.88 (item 0 is then 200.0,
  166.8, 100.1 and 83.4 ms before it), and the record runs 2, 3, 4 and 6 items past the last
  frame. The IMU (record 3) starts within 17 ms of record 4's first item, so 92 to 190 ms
  before field 24, and ends within 48 ms of its last. An LRV's records 3 and 4 are its
  master's byte for byte (above), so the same holds there. Times in ms:

  | Take | Rate | Frames a track | Record 4's items | Field 24's item | Items past the last frame | IMU's first − item 0 | IMU's first − field 24 | IMU's last − last item |
  |---|---|---|---|---|---|---|---|---|
  | 004 | 29.97 | 114 | 122 | 5 | 3 | +10.5 | −156.4 | +15.3 |
  | 005 | 25 | 116 | 123 | 5 | 2 | +9.8 | −190.2 | +47.8 |
  | 006 | 59.94 | 190 | 200 | 6 | 4 | −12.3 | −112.4 | +9.9 |
  | 007 | 119.88 | 586 | 602 | 10 | 6 | −8.2 | −91.6 | −7.9 |
  | 008 | 29.97 | 966 | 974 | 5 | 3 | +16.3 | −150.5 | +10.0 |
  | 009 | 29.97 | 1904 | 1912 | 5 | 3 | +10.9 | −155.9 | +10.4 |
  | 010 | 29.97 | 968 | 976 | 5 | 3 | +15.4 | −151.5 | +10.2 |
- **Calibration strings**: all four generations, **byte-identical in all 7 masters and all 7
  LRVs**, so in every mode and take (2026-11-01): they belong to the camera unit. V1, V2 and
  V3 share their centres and angles; V6 is a separate calibration. All four below, copied from
  fields 5, 53, 54 and 111 and laid out as in §4.2 under §4.1's labels (which the bytes don't
  all settle, §4.1), each number its token as stored: ξ and t to 6 decimals, the radius, focal,
  centre and angles to 3, the distortion terms to 8. M3's plan (7.1) and synth's lens tests
  (`crates/synth/src/lens.rs`) take their numbers from here, and camera's tests
  (`crates/camera/tests/strings.rs`) the four strings whole.

```
V1 (word 0x471: generation 0, body 4, lens type 113; 16 tokens)
  lens 0: radius 2653.545  cx 2701.130 cy 2692.130  ypr -0.198 0.211 90.280
  lens 1: radius 2661.203  cx 8069.000 cy 2684.950  ypr 0.239 0.113 89.107
  canvas 10752 5376
V2 (word 0x20400; 34 tokens)
  lens 0: radius 2660.372  cx 2701.130 cy 2692.130
          ypr -0.198 0.211 90.280  t 0.000000 0.000000 0.000000
          c0..c3: 1.00000000 -0.18756898 0.27067676 -0.08894466
          10752 5376 113
  lens 1: radius 2670.937  cx 8069.000 cy 2684.950
          ypr 0.239 0.113 89.107  t -0.004460 0.000017 -0.031920
          c0..c3: 1.00000000 -0.18880324 0.27227741 -0.08904964
          10752 5376 113
V3 (word 0x30400; 40 tokens)
  lens 0: xi 2.000000  fx 4284.620 fy 4284.280  cx 2701.130 cy 2692.130
          ypr -0.198 0.211 90.280  t 0.000000 0.000000 0.000000
          k1 0.18632051 k2 2.04984355 k3 -3.24446917  p1 -0.00143576 p2 -0.00339240
          10752 5376 113
  lens 1: xi 2.000000  fx 4291.030 fy 4291.030  cx 8069.000 cy 2684.950
          ypr 0.239 0.113 89.107  t -0.004460 0.000017 -0.031920
          k1 0.18940836 k2 2.07073069 k3 -3.24103332  p1 0.00013831 p2 -0.00299873
          10752 5376 113
V6 (word 0x60400; 56 tokens)
  lens 0: xi 2.000000  fx 4277.520 fy 4277.270  cx 2713.050 cy 2689.210
          ypr -0.310 -0.271 90.255  t 0.000000 0.000000 0.000000
          d0..d12: 0.28032997 0.89045405 2.04927588 -8.05179787 0.00000000
                   0.00120200 -0.00259487 0.02324706 0.00533920
                   -0.01560208 0.00019357 -0.04580523 0.00197187
          10752 5376 113
  lens 1: xi 2.000000  fx 4284.400 fy 4284.760  cx 8075.730 cy 2680.350
          ypr 0.120 -0.173 89.112  t 0.000637 0.000001 -0.031525
          d0..d12: 0.27665332 0.98522973 1.83605266 -7.99374485 0.00000000
                   0.00021056 -0.00147626 0.01940425 0.00331838
                   -0.01330665 0.00074511 -0.02859299 0.00888746
          10752 5376 113
```

Open questions this answers: Q1 (model and firmware), Q13 (25 fps timebase; no `_10_` on the
X5), Q14 (every mode is one file plus an LRV), Q16 (thumbnail record), and the LRV half of Q2
(left = track 1). Samples now exist for Q7 and Q8 (clips 9 and 10) and for the seam half of
Q4–Q6 (clip 8), but not yet the Studio export they need as a reference. The rest of Q2, which
track is the screen-side lens, needs a take with one lens covered (take 15), which clip 8
doesn't have. Still missing: I-Log/10-bit (Q11), 360 audio (Q15), FlowState on/off (Q9), a
Studio export (Q4–Q6), and a split recording (Q12).

## 0. How to read this

Every claim carries a tag and a source key.

| Tag | Meaning |
|---|---|
| **[bytes]** | I checked it on real camera bytes (only the GyroView X5 fixtures) |
| **[code]** | What a parser's source code does. That is evidence of a format, not proof of it |
| **[reported]** | Another project says it checked this on its own files; I have not seen those files |
| **[vendor]** | Stated by Insta360 in a public manual or spec page |
| **[unverified]** | A guess, desk research, or one source with no corroboration |
| **[conflict]** | Sources disagree; both readings are given |
| **[RE]** | The source says it derived this from disassembling Insta360 Studio or from vendor SDK files. Read section 8 before relying on it |

Source keys such as `TP mod.rs:84` mean *telemetry-parser, `src/insta360/mod.rs`, line 84, at
the pinned commit*. The full list is at the end.

## 1. The short version

1. An `.insv` file is an ordinary ISO-BMFF/MP4 file (H.264 or HEVC, AAC) with a proprietary
   **trailer** appended after `moov`. The trailer ends in a 72-byte footer whose last 32 bytes
   are the ASCII magic `8db42d694ccc418790edff439fe026bf`. FFmpeg plays the video tracks; we
   read the trailer ourselves. [bytes] [code] `TP mod.rs:19-20,81`
2. There are three video layouts. The first is **one file per lens** (`_00_` and `_10_`): ONE X,
   ONE X2 and X3 at 5.7K, plus the ONE RS 1-inch 360 and the ONE R 360 module. The second is
   **one file with one 2:1 track holding both fisheyes side by side**: the same older cameras below
   5.7K, and every LRV. The third is **one file with two square HEVC/H.264 tracks**, one per lens:
   X4, X4 Air, X5, X6 and Antigravity A1. [vendor] [reported] (section 2)
3. The trailer is a run of records. Each record is a payload followed by a 6-byte footer
   `{u8 format, u8 id, u32le size}`. Older cameras are walked backwards from the end. X4 and later
   put an **index record (id 0)** last, with fixed 10-byte slots, and pad records to alignment
   boundaries. Everything is little-endian. [bytes] [code]
4. Record 1 is a **protobuf** message holding model, firmware, timing, the IMU configuration,
   the stream layout and the per-lens **calibration strings**: on Pat's X5 92 top-level fields
   numbered up to 163 on a master, and on an LRV the same but 79 and 80 (Pat's X5 samples),
   where this list first said about 80, numbered up to 199.
   Record 3 is the IMU, record 4 the per-frame exposure. [bytes] [code]
5. Calibration comes in four generations of underscore-separated strings: V1, V2, V3 and V6.
   The modern ones (V3 with 19 tokens per lens, V6 with 27) describe a **Mei unified
   omnidirectional model** (`xi` = 2.0 on the X5) with Brown-style distortion. Each also carries
   per-lens yaw, pitch, roll and a translation in metres. [bytes] [code]
6. **Which track is which lens is not obvious.** On the X5, fields 131 = 4 and 80 = 1 say
   track 0 holds the screen-side lens. Most sources read it that way; one does not. [conflict]
   (section 2.6)
7. On the X5 the IMU runs at about 1 kHz as raw `u16` counts (accelerometer ±32 g, gyro
   ±2000 deg/s), timestamped in µs on the same clock as the per-frame exposure record. The
   first video frame is the exposure entry equal to field 24. [bytes]
8. **telemetry-parser** is MIT OR Apache-2.0, so its licence is compatible with
   GPL-3.0-or-later. But crates.io only has 0.2.6 from 2022, which lacks X4, X5, X6 and the A1.
   Its current geometry module is derived from Insta360 Studio disassembly. Section 8 recommends
   writing our own trailer reader and using telemetry-parser only as a test oracle, or as a
   pinned git dependency for the parsing alone. (M3: not used, not even as an oracle, by
   `docs/design/m3-plan.md` §20 and the plan's D62 (§23), which DECISIONS.md records at M3.21.
   M4 still decides whether it parses the IMU.)

## 2. Container layout

### 2.1 What every `.insv` has in common

- **Box order** on the X5 is `ftyp` (28 B), `mdat` (64-bit largesize header, 16 B), `moov`, then
  the trailer wrapped in a top-level `inst` box. `inst.size` = trailer size + 8. [bytes]
  `GV test/fixtures/x5/office/box-headers.json`. GyroView says the `inst` wrapper appears from
  X5 firmware v1.7 on, and that older firmware appends the trailer bare. [reported]
  `GV docs/FORMAT.md` [conflict] Pat's X5 at firmware v1.1.22 already writes the `inst` box,
  on all 14 files [bytes] (Pat's X5 samples), so no X5 firmware is yet known to append it
  bare. orbis-reframe lists a `wide` box after `ftyp`; the fixture has none.
  [conflict, minor] `OR docs/formats/insta360-x5.md`
- On the **ONE X2** there is no `inst` header. `moov` is followed by a `free` box that pads to
  the next 1 MiB boundary, and the trailer starts right at that boundary. [reported]
  `OR insta360-one-x2.md §2` ExifTool also notes "Insta360 sometimes puts free block before
  trailer". `ET lib/Image/ExifTool/QuickTime.pm`
- `moov` sits at the end, after `mdat`. There are no fragments, one sample per chunk, `co64` on
  large files, and on multi-track files the tracks' samples alternate in file order (a frame of
  one lens, then the other, with audio between). [reported] `GV docs/FORMAT.md §Movie box`
- Handler names are `INS.HVC`, `INS.AVC` and `INS.AAC` on the X5, and `Ambarella AVC` /
  `Ambarella AAC` on the ONE X2. The X2's `udta` holds an `AMBA` box. [reported]
  `OR insta360-x5.md §2`, `OR insta360-one-x2.md §2`
- **Recordings split** every 29 min 59 s on the X5. [reported] `OR insta360-x5.md` How the
  pieces are named, and whether field 26 links them, is not documented anywhere I found.
  [unverified] (open question Q12)

### 2.2 Layouts per model

"Files" means VID files per take, not counting the LRV proxy.

| Model | 360 video layout | Track(s) | Trailer carrier | Trailer style | Evidence |
|---|---|---|---|---|---|
| ONE X (2018) | 5.7K: 2 files `_00_` and `_10_`. Below 5.7K: 1 file | per lens; below 5.7K presumably one packed 2:1 track | presumably `_00_` | walked backwards (ExifTool has read ONE X files since 2019) | [vendor] `IM fileformat`; the rest [unverified] |
| ONE X2 | 5.7K: `_00_` + `_10_` + `LRV_…_11_…`. 4K and 3K: 1 file | 2880² H.264, one per file | `_00_` and the LRV only. **`_10_` has no trailer** | raw after `free`, no index, walked backwards | [reported] `OR insta360-one-x2.md §1-3` (15 takes) |
| X3 | 5.7K: 2 files. 4K and 3K: 1 file | per lens at 5.7K; one 2:1 packed track below | `_00_`. `_10_` has no trailer on the one pair seen | walked backwards; the metadata record comes last | [vendor] `IM fileformat`; [reported] `RF docs/insta360.md` (X3 photos, one proxy, one split pair) |
| ONE R, 360 module | one file per lens at 5.7K | per lens | ? | contiguous, walked backwards | [reported] `GV adr/0009` (one local sample), `GV FORMAT.md` (insta360py fixture) |
| ONE RS 1-inch 360 | one file per lens at 6K | 3072² H.264, 24 fps on the one clip seen | ? | ? | [reported] `GV FORMAT.md`, `GV adr/0009` (one local sample) |
| X4 | always 1 file | two square tracks | the one file | **indexed**, 4 KiB alignment seen | [vendor] `IM fileformat`; [reported] `RF docs/insta360.md` (two X4 files) |
| X4 Air | 1 file + LRV | two square tracks | the one file | probably indexed | [vendor] `IM fileformat`; the rest [unverified] |
| X5 | 1 file + `LRV_…_01_….lrv` | two square HEVC tracks | the one file, and the LRV | **indexed**, 31 slots, 128 KiB alignment | [bytes] `GV fixtures/x5`; [reported] `OR insta360-x5.md`, `GV FORMAT.md` |
| X6 | 1 file + LRV | two 3840² HEVC Main 10 tracks | the one file | indexed, 59 slots | [reported] `GV FORMAT.md`, `OR insta360-x6.md` (contributor clips) |
| Antigravity A1 (drone) | 1 file (+ `.prx`) | two 3840² HEVC tracks | the one file | indexed, **256 KiB slots with no record footers** | [code] `TP mod.rs:99-116`; [reported] `GV FORMAT.md` |

Insta360's file-format page says, verbatim: "Unlike X3 which generates two VID files, X6, X5,
X4 Air, and X4 generate only one VID file containing data from both lenses". It also says: "(For
X3, X2, and ONE X only) LRV files will not be generated in the following modes: Bullet Time,
TimeShift, Timelapse, Video (4K@60/50fps, 3K@100fps, or frame rates above 100fps)". [vendor]
`IM fileformat`

### 2.3 File names

`[prefix]_[YYYYMMDD]_[HHMMSS]_[{00,01,10,11}]_[NNN].[ext]`. In the two-digit field, the first
digit is "0 … the lens on the back (away from the screen)" and "1 … the lens on the screen side".
The second digit is "0 … master file; 1 … proxy file". [vendor] `IM fileformat`

- `VID_` is normal video. `LRV_` is the proxy. `PRO_VID` is used for FreeFrame. [vendor]
  `IM fileformat`
- On a one-file camera the master is still named `_00_`, and its proxy `LRV_…_01_….lrv`.
  [bytes] (the fixture source name `VID_20260814_132640_00_013.insv` in `GV
  fixtures/x5/manifest.json`); [reported] `GV FORMAT.md`
- The **ONE X2's proxy is `LRV_…_11_NNN.insv`**: extension `.insv`, lens digit 1, yet it holds
  both lenses. [reported] `OR insta360-one-x2.md §1`; the same name pattern appears in
  `IV` insvtools' README example `LRV_…_11_677.insv`.
- Clips trimmed in the Insta360 app gain a `-trim` suffix after the sequence number. [reported]
  `GV FORMAT.md`

### 2.4 Pairing two-file recordings, and frame lock

What the sources agree on:

- Lens 0 is the `_00_` file (back lens) and lens 1 is `_10_` (screen-side lens).
  [vendor] [reported] `RF docs/insta360.md`
- On the ONE X2 and X3, only `_00_` and the LRV carry a trailer. **The `_10_` file cannot be
  identified from its contents**, so it has to be found by name next to its `_00_`. [reported]
  `OR insta360-one-x2.md §1`, `RF docs/insta360.md`
- Frame *n* of `_00_` is frame *n* of `_10_`. Both start at pts 0. At 25 fps on the X2, `_10_`
  (and the LRV) is **one frame shorter** than `_00_`; at 29.97 fps the lengths are equal.
  orbis-reframe measured this by seam correlation at shifts of −2 to +2 frames. [reported]
  `OR insta360-one-x2.md §3.1`
- The X2's audio in `_00_`, `_10_` and the LRV decodes to identical samples. [reported]
  `OR insta360-one-x2.md §3.1`
- Field 26 `file_group_info {type, index, identify, total}` is meant to describe the group.
  telemetry-parser builds a sibling name from it only when `total == 2`, by replacing the
  two-digit lens field with `"{other}0"`. [code] `TP rig.rs:637-660` On the X2, however, field 26
  is `{type 6, index 0, identify ""}`, which names nothing. [reported] `OR insta360-one-x2.md
  §1` On the X5 it is `{type 0, index 0, identify "/DCIM/Camera01/VID_…_00_014.insv", total 0}`.
  [bytes] (sailing fixture)

Proposed rule for Spherecut. Group files by `(prefix, date, time, seq)`; take lens 0 = `_00_`
and lens 1 = `_10_`; read the trailer from whichever file has one, which on known cameras is
`_00_` (GyroView does the same: "that file is lens 0 whatever it is called", `GV FORMAT.md`).
Then check that both files have the same timescale, frame rate and first pts, pair frames by
index, and set the clip length to the shorter track. Warn when a `_10_` file has no `_00_`.

### 2.5 Multi-track files

The two video tracks share frame rate and size and are frame-locked. [reported]
`OR insta360-x5.md §3.1` On the X5 the tracks' own tables say so [bytes]: on all 7 of Pat's
masters, both lenses have the same timescale, frame count, first pts (0), edit list (one edit
from media time 0), `stts` (one run of 1001 ticks, or 1000 at 25 fps), `ctts` (every offset 0)
and `stss` (a keyframe every 2 s at 29.97, 25 and 59.94 fps, every 1 s at 119.88), and each
LRV's first frame is at its own movie's start, as its master's is. So the container gives no
offset between the lenses, and P-TRK's R1 is not taken (`media::mp4::track_timing` and the
Tier 2c test `samples_x5_tracks_equal`, M3.0, 2026-11-01). Whether the pictures agree is
P-TRK's pixel half (`samples_lens_offset_is_zero`, M3.6), and whether an LRV began with its
master is M3.14's `lrv_matches_master`. The X5 can carry **two AAC streams** when an external mic is
connected, and nothing in the container says which is which. [reported] `OR insta360-x5.md
§3.1`

### 2.6 Which track is which lens [conflict]

Three metadata fields describe the layout.

| Field | Values | Source |
|---|---|---|
| 131 `stream_type` | 0 Unknown, 1 SingleStreamFile, 2 DualStreamFile, 3 DualStreamTrack ("lens order == track order"), 4 DualStreamTrackReverse | [code] `TP extra_info.rs:372-381` (the enum names were "read out of [the vendor's] embedded schema", `TP rig.rs:610`) [RE] |
| 79 | 1 split files, 2 multi-track | [reported] `GV FORMAT.md` (marked provisional), `IR docs/INSV_FORMAT.md` |
| 80 | 1 = track 0 is stream `10`, 2 = track 0 is stream `00` | [reported] `IR docs/INSV_FORMAT.md`; "field 80 = 1 meaning track 0 holds lens 1", seen on two X4 files [reported] `RF docs/insta360.md` |

Both X5 fixtures have **131 = 4, 79 = 2, 80 = 1**. [bytes]

- telemetry-parser maps 131 = 4 to `TrackReversed`: lens *i* is read from track `num−1−i`, so
  calibration lens 0 is track 1. It adds that this reading "has not been confirmed against a
  front/back picture". [code] `TP rig.rs:606-626,689`
- raw360-formats and insta360-rs read field 80 the same way: track 0 holds the screen-side lens.
  [reported]
- Proxies agree with that reading. In a packed frame, lens 0 (the back lens, `_00_`) is the left
  half on the ONE X2 [reported] `OR insta360-one-x2.md §3.2` and in GyroView's layout detector
  [code] `GV packages/core/src/domain/format/layout/detectLensLayout.ts`. On the X5 LRV, `.insv` stream 0 is the
  **right** half [reported] `OR insta360-x5.md §3.2`, and Pat's samples show it [bytes]: the
  left half is track 1, the right half track 0 (Pat's X5 samples). That order is a fact. That
  X5 stream 0 is therefore the screen-side lens rests on the X5 packing its back lens on the
  left as the ONE X2 does [unverified], so it stays open until a take with the screen-side
  lens covered (take 15, Q2).
- orbis-reframe nonetheless binds calibration lens 0 to stream 0 on the X5
  (`OR insta360-x5.md §3.1, §5.3`), and needs a per-clip lens-1 yaw fix of a fraction of a
  degree. That would be consistent with swapped calibration blocks. [unverified]

**Spherecut should read field 131, then field 80, and treat the reversed binding as the
default for the X4 and X5.** It must confirm this on a sample by covering one lens (Q2).

## 3. The trailer

### 3.1 The 72-byte tail

| Offset from EOF | Size | Field | Source |
|---|---|---|---|
| −72 | 32 | zeros | [bytes] |
| −40 | u32 le | **trailer size** E: bytes from the payload start to EOF, these 72 included, the 8-byte `inst` header excluded | [bytes] (E = 20 606 167, `inst.size` = E + 8) |
| −36 | u32 le | version, **3** on every camera documented | [bytes]; `GV FORMAT.md`; `IV` insvtools refuses anything else |
| −32 | 32 ASCII | `8db42d694ccc418790edff439fe026bf` | [bytes] `TP mod.rs:20` |

`payload_start = file_size − E`. [bytes] (`6 894 571 870 − 20 606 167 = 6 873 965 703`, which
is also the value of metadata field 9 `file_size`, matching `OR insta360-x5.md`)

telemetry-parser detects the format by the magic at EOF, not by extension, and accepts `mp4`,
`mov`, `insv`, `lrv` and `prx`. [code] `TP mod.rs:53-66` ExifTool reads the last 78 bytes, checks
the magic and takes `unpack('x38V')` as the size. [code] `ET QuickTimeStream.pl ProcessInsta360`

An older "ExtraInfo V2" trailer exists for the phone-attached Nano, Nano 2, Air and Air 2
families. It has a 40-byte header, JSON metadata and a gyro header with magic
`9c792b1ac55c40418d36ffb0d1d16b58`. This is out of scope. [unverified] `IR docs/INSV_FORMAT.md`

### 3.2 Records

Each record is `payload ‖ footer`, where the footer is 6 bytes: `u8 format` (0 binary,
1 protobuf, 2 JSON), `u8 id`, `u32le payload_size`. The footer of the record nearest the tail
sits at EOF−78. [bytes] [code] `TP mod.rs:88-97,141-160`

**Sequential layout** (ONE X2, X3, ONE R and older): walk backwards from EOF−78. Read the footer,
step back over the payload, read the next footer, and continue until reaching `payload_start`.
[code] `TP mod.rs:141-162` On the X3 the metadata record comes last, i.e. it is found first.
[reported] `RF docs/insta360.md`

**Indexed layout** (X4, X5, X6, A1): the record at EOF−78 has id 0 (format 0). Its payload is a
table of 10-byte slots `{u8 id, u8 format, u32le size, u32le offset_from_payload_start}`. Slot *k*
describes record type *k*, and an all-zero slot means the type is absent. [bytes] [code]
`TP record.rs:58-73` On both X5 fixtures the index is 310 bytes, i.e. 31 slots: the 388 bytes
from the index start to EOF = 310 + 6 + 72. [bytes] The X6 uses 59 slots. [reported]
`GV FORMAT.md` Note the field order: the **id comes first in a slot but second in a footer**.

- X5: records sit at absolute file offsets aligned to **128 KiB** [bytes] (e.g. record 2 at
  6 893 338 624 = 52 592 × 131 072). The gaps between them are **not zero padding**: all 14 of
  Pat's files have stray non-zero bytes in them, mostly in the gaps before records 9, 4 and 2
  [bytes] (Pat's X5 samples, which count them). The metadata record and record 10 are not
  aligned: record 1's footer ends where the index starts, at EOF − 388, on all 14 of Pat's
  files, and the index's own footer follows the index, at EOF − 78 [bytes]. Each record still
  ends in its 6-byte footer. [bytes] [code] `TP mod.rs:99-116`
- X4: 4 KiB alignment was seen. [reported] `RF docs/insta360.md`
- **Antigravity A1**: each record begins a 256 KiB slot and there are **no footers at all**, so the
  table is the only index. A reader that insists on footers sees an empty file. [code]
  `TP mod.rs:99-116`
- Spherecut should trust the table entry, bounds-check it against the trailer, and skip
  entries that run past it, as telemetry-parser does. [code] `TP mod.rs:122-127`
- **Parse record 1 first.** Whether the IMU is raw or float, and its full-scale ranges, live in
  record 1 (telemetry-parser sets state from it, `TP record.rs:102-110`).

ExifTool reads the footer's `{format, id}` as one little-endian `u16`, so its "0x300" means
format 0, id 3. [code] `ET QuickTimeStream.pl` `%insvDataLen`

### 3.3 Record types

Decimal id. "Seen on X5" refers to the fixture indexes. [bytes]

| id | Name (telemetry-parser) | Other names | Format | Seen on X5 | Notes and sources |
|---|---|---|---|---|---|
| 0 | Offsets | index | bin | yes | §3.2 |
| 1 | Metadata | info | protobuf | yes, about 4 KB | §3.5 |
| 2 | Thumbnail | preview | bin | yes, 1.2 MB | X2: an H.264 IDR frame per lens [reported] `OR insta360-one-x2.md §5`. X5: a 1280×640 NV12 stitched equirect after a 40-byte header [reported] `OR insta360-x5.md`. ExifTool: a JPEG or TIFF preview with a 40-byte header [code] |
| 3 | Gyro | IMU | bin | yes | §3.4 |
| 4 | Exposure | | bin | yes | §3.4 |
| 5 | ThumbnailExt | | bin | | [code] |
| 6 | TimelapseTimestamp | per-frame timestamps | bin | | `u64` µs per frame (timelapse) [code] `TP record.rs:223-232`; [reported] `OR insta360-x5.md` |
| 7 | Gps | | bin | | 53 B, 10 Hz records with 1 Hz fixes, only with a GPS source [code] [reported] |
| 8 | StarNum | | | | [code], unparsed |
| 9 | AAAData | AE/AWB | bin | yes | 48 B items [code] `TP record.rs:263-297` |
| 10 | Anchors | highlights | bin | yes, 35 B | [code] `TP record.rs:298-335` |
| 11 | AAASimulation | | | yes, 4.2 MB | unparsed |
| 12 | ExposureTimeVin1 | | bin | sailing (8K30) only | "The secondary stream's exposure times, same layout as Exposure" [code] `TP record.rs:27-28`. Same size as record 4 on the sailing clip [bytes] |
| 13–18 | Magnetic, Euler, SecGyro, Speed, TBox, Editor | | | | names only [code] |
| 19–30 | (not in TP) | heart rate, forward direction, up view, **22 shell recognition**, position, 24 timelapse quat, 25 quaternion, APEI, **27 dynamic ISP**, **28 static ISP**, **29 AE flicker**, vehicle info | | 22, 28, 29 (27 on sailing) | names only, from `IR docs/INSV_FORMAT.md` [unverified]. Record 22 is 8 306 836 B on both X5 fixtures [bytes]; on Pat's 7 takes record 22 is 8 306 836 B and record 28 12 793 B, with different bytes in every take [bytes] |
| 24 | TimelapseQuat | | bin | | 40 B `{f64 t, f64 w,x,y,z}`, 11 per frame, on X5 timelapse [reported] `OR insta360-x5.md` |
| 25 | Quaternion | | | | name only [code] |
| 51 | OnlineQuaternion | | | | name only [code] |
| 52, 54 | | | bin | X6 only | 16 B `{u64 µs, f64 s}` at 25 Hz, probably per-lens LRV exposure [reported] `GV FORMAT.md` |
| 57 (0x39) | | trim | bin | app-trimmed clips | 28 B: `"TEO1"`, two `u16`, a `u32le` frame index of the original [reported] `GV FORMAT.md` |
| 128 (0x80) | TimeMap | | bin | app-trimmed or edited clips | §3.4 [code] `TP record.rs:75-100` |

### 3.4 Binary record layouts (all little-endian)

**Gyro (3).** There are two forms, chosen by metadata field 62 `is_raw_gyro`. [code]
`TP record.rs:158-207`

- Raw (X5: field 62 = 1): 20 B items `{u64 ts, u16 ax, ay, az, gx, gy, gz}`, each value
  offset-binary (`v − 32768`). Scale is `32768 / range`, with the ranges in field 65
  `{1: acc_range g, 2: gyro_range deg/s}`. Units are g and deg/s. telemetry-parser defaults to
  16 g and 2000 deg/s when field 65 is missing. [code] **The X5 writes 32 g and 2000 deg/s**:
  the resting accelerometer magnitude is about 1022 counts, i.e. 0.998 g at 1024 counts per g.
  With the 16 g default it would read 0.5 g. [bytes] ExifTool scales raw values by
  `(v−0x8000)/1000`, which is wrong. [code] `ET QuickTimeStream.pl`; agreeing [reported]
  `OR insta360-x5.md` insta360-rs gives an 8 g default. [unverified]
- Float (ONE X2, older): 56 B items `{u64 ts, f64 ax, ay, az (g), f64 gx, gy, gz (rad/s)}`.
  [code]; [reported] `OR insta360-one-x2.md §8` (500 Hz, ts in ms)
- **Accelerometer first, then gyro**, in both forms. [code] [bytes] (the resting magnitude fits
  the first three values)
- ExifTool chooses 20 or 56 B items by `len % 20` / `len % 56`. [code]
- X5 rate: timestamp deltas of 1000–1028 µs (mean 1001.3 µs, about 999 Hz) over the first 2000
  samples [bytes]. orbis reports about 1006 Hz, irregular [reported].

**Exposure (4, 12).** 16 B items `{u64 ts, f64 shutter_seconds}`, one per frame. [code]
`TP record.rs:210-222` On the X5 office clip (5.7K60) the deltas are 16.67–16.69 ms and the
shutter is 1/640 s. [bytes]

**Timestamp units differ by model.** On the X5 the IMU and exposure timestamps and field 24
are in **µs**. [bytes] On the ONE X2 the IMU timestamps and field 24 are in **ms**. [reported]
`OR insta360-one-x2.md §8` telemetry-parser has no per-model unit table: it divides by an extra
1000 when `is_raw_gyro` is set [code] `TP mod.rs:234-262`. So raw IMU is assumed to mean µs and
float IMU to mean ms. [unverified as a general rule]

**GPS (7).** 53 B items: `u64 unix_s`, `u16 ms`, `char fix (A/V)`, `f64 lat`, `char N/S`,
`f64 lon`, `char E/W`, `f64 speed_m_s`, `f64 track_deg`, `f64 altitude_m`. [code]
`TP record.rs:233-262`; ExifTool and orbis agree [reported] `OR insta360-x5.md §8`

**AAAData (9).** 48 B items: `u32 ts`, `f32 ev_target`, `f32 exp_time`, `u32 data_stat`,
`u32 luma_struct` (`iso = (100·(luma>>19))>>6`), `7×u32`. [code] `TP record.rs:263-297`

**Anchors (10).** Repeated `{u8 type, u32 count, count × u64}`; types 2 and 18 use pairs of
`u64`. [code] `TP record.rs:298-335`

**TimeMap (128).** `u32 unk, u32 num_trims, u32 unk, u32 count`, then `num_trims × 4 f64`, one
`f64`, then `count × (f64 t, f64 v)`. Gyroflow uses `t − v` (ms) per frame as an extra sync
offset. [code] `TP record.rs:75-100`, `GF src/core/gyro_source/mod.rs:609`

**Gyro calibration (field 31, inside record 1).** 56 B: `6 × f64` followed by a `u64` unix
timestamp. [code] `TP extra_info.rs:56`, `parse_gyro_calib`. The meaning of the six values
(presumably biases) is [unverified].

### 3.5 The metadata record (id 1, protobuf)

The field names come from telemetry-parser's prost schema [code] `TP src/insta360/extra_info.rs`,
which ultimately mirrors the vendor schema [RE]. "X5" values are from both fixtures [bytes];
"X2" values are from `OR insta360-one-x2.md` [reported].

| # | Name | Type | Notes and observed values |
|---|---|---|---|
| 1 | serial_number | string | |
| 2 | camera_type | string | `"Insta360 X5"`. telemetry-parser keys its per-model tables on this string, e.g. `"Insta360 OneRS"`, `"Insta360 ONE X2"` |
| 3 | fw_version | string | X5 `v1.7.43_build1`, `v1.11.6_build1` |
| 5 / 17 | offset / original_offset | string | **V1** calibration (§4). ExifTool shows it as "Parameters" |
| 7 | creation_time | varint | X5 `20260814132640` (decimal-coded local date and time) |
| 9 | file_size | varint | = payload start of the trailer [bytes] |
| 10 | total_time | varint | seconds (X5 262, 194) |
| 19 | dimension | {x, y} | coded size of **one** track: X5 2880² (5.7K60), 3840² (8K30) |
| 20 | frame_rate | | X5 60, 30 |
| 22 | gamma_mode | string | X5 `standard`; `I_Log` on I-Log clips [reported] `OR insta360-x5.md`; X6 `standard` / `I_Log` / `Dolby_Vision` [reported] `OR insta360-x6.md`; X2 `linear` [reported]. GyroView calls this field "capture mode" [conflict, naming] |
| 24 | first_frame_timestamp | int64 | clock of the first video frame: X5 µs, X2 ms (§5.3) |
| 25 | rolling_shutter_time | double | ms: X5 8.4075 (5.7K60) and 21.244 (8K30); orbis also reports 11.837 on another clip; X2 36.94. On Pat's X5 an f32 widened to the double: 21.244001388549805 at 8K, 9.181875228881836 at 5.7K60 HDR, 5.53125 at 4K120 [bytes] |
| 26 | file_group_info | {1 type SubMediaType, 2 index, 3 identify, 4 total} | §2.4. SubMediaType 0–17 = VideoNormal, Bullettime, Timelapse, PhotoNormal, PhotoHdr, PhotoInterval, VideoHdr, Burst, StaticTimelapse, Timeshift, AebNight, SuperNormal, Looprecording, Starlapse, PanoMode, Fpv, Movie, Slowmotion [code] |
| 27 | window_crop_info | {1 src_w, 2 src_h, 3 dst_w, 4 dst_h, (5, 6 offset x, y)} | X5 `5376, 5376, 5312, 5312, 0, 0` [bytes]; X2 `3040 → 2880` [reported]; X6 `7744 → 7680` [reported]. Fields 5 and 6 come from GyroView only |
| 28 | gyro_timestamp | double | ms; **X5 1.6** [bytes], **X2 −12.0** [reported]. Used only if 29 is set |
| 29 | is_has_gyro_timestamp | bool | X5 1 |
| 31 | gyro_calib | bytes | §3.4 |
| 40 | total_frames | | |
| 42 | is_flowstate_online | bool | X5 0. Gyroflow uses it to label in-camera EIS [code] `GF src/core/camera_identifier.rs` |
| 44 | (video size) | {1 width, 2 height} | The file's own video: one track's on a master (Pat's X5 3840² at 8K, 2880² at 5.7K, 1920² at 4K), the whole frame's on an LRV (1664 × 832) [bytes] |
| 46 | cam_posture | enum 0–3 = 0/90/180/270° | X5 office 3, sailing 0 [bytes] |
| 47–49 | fov_type, distance, fov | | X5 15, 0.6, 56.82 (probably the app's reframe defaults) [unverified] |
| 51 | gyro_type | enum 0 ICM20948, 1 ICM40609 | X5 1 |
| 52 | media_data_rotate_angel (sic) | 1 = 0°, 3 = 180°, 6 = 90°, 8 = 270° | EXIF-orientation-style codes [code] `TP extra_info.rs:291-297` |
| 53 / 55 | offset_v2 / original | string | V2 |
| 54 / 56 | offset_v3 / original | string | V3 |
| 61 | audio_mode | enum | X5 9 (outside telemetry-parser's enum) [reported] |
| 62 | is_raw_gyro | bool | X5 1; absent on X2 |
| 64 | pts_type | 0 unknown, 1 Mp4, 2 ExposureFile | X5 2: frame times come from the exposure record |
| 65 | gyro_cfg_info | {1 acc_range, 2 gyro_range} u32 | X5 `{32, 2000}` [bytes] |
| 68 | (optical state) | | X5 office 0, sailing 4. insta360-rs: 0 bare, 1 protector, 2 dive, 4/5 guards, … [unverified] |
| 79, 80 | layout, track order | | §2.6. 2 and 1 on Pat's X5 masters; absent on their LRVs [bytes] |
| 93 | (a length) | varint | ms, but not the clip's length: on Pat's X5 ⌊*n* · 1000 · *den* / *num*⌋, with *n* one more than the frames in each of the master's tracks and *num/den* the master's rate, on the master and its LRV alike (004: 3837 for 114 frames at 30000/1001; LRV 006: 3186 from its master's 190 at 60000/1001, not its own 95) [bytes] (Pat's X5 samples) |
| 98 | | message | X5 sailing `{1: 393534973, 2: 444202729, 3: 1789712955321, 4: 1789713149154, 5: 5815}`, where 3 and 4 are unix ms about the clip's length (193.8 s) apart and item 2 is not "last frame µs" as orbis says (only 50.7 s after item 1) [bytes] [conflict]. On Pat's X5 [bytes] (Pat's X5 samples), with *n* as for 93 and *r* the master's rate rounded up (30, 25, 60, 120), on the master and its LRV alike: item 1 = field 24 (µs); item 2 = item 1 + ⌊*n* · 10⁶ / *r*⌋ µs; item 3 is unix ms; item 4 = item 3 + ⌊*n* · 1000 / *r*⌋, which isn't the last frame's time (004: 3833 ms after item 3, where its last frame starts at 3770 ms); item 5 is *n* on a master and the LRV's own frame count on an LRV, so not the clip's frame count (004: 115 for 114 frames a track); items 6 and 7 are 0. The sailing file's item 2 doesn't fit that rule, so item 2's meaning stays [conflict] |
| 111 / 112 | offset_v6 / original | string | V6 |
| 129 | image category | | X5 2 ("double fisheye"; 6 = equirect) [unverified] `IR` |
| 130 | rotation | | X5 1 (= 0°) [unverified] `IR` |
| 131 | stream_type | enum | §2.6 |
| 132 | codec | 1 H.264, 2 H.265, 3 MJPEG, … | X5 2 [unverified meaning] `IR` |
| 133, 134 | | | 133 absent and 134 = 0 on all 14 of Pat's files; meaning unknown [bytes] |
| 135 | bitrate | Mb/s | X5 200 (5.7K60), 180 (8K30) |
| 136 | capture_offset_version | enum 1 V1, 2 V2, 3 V3, **4 V6**, 5 V8 | X5 4. telemetry-parser treats it as advisory and always prefers V6 > V3 > V2 > V1 [code] `TP extra_info.rs:132-136`. (GyroView says no camera writes 136; the X5 fixtures do [bytes] [conflict]) |
| 145 | lens profiles | message | X5: per accessory (`bare`, `ProtectorA`, `ProtectorS`, `ProtectorAS`, `InvisibleDiveWater`, `InvisibleDiveAir`) a degree-5 polynomial in degrees (bare: `0, 0.03159, −8.415e−5, 2.201e−6, −1.284e−8, 0`) and a field of view (bare 200°, protectors 195°, dive 190°) [bytes]. orbis reads the polynomial as image height in mm [reported]. Absent on X2 and X6 [reported] |

Fields above 65 are not in telemetry-parser's schema except 111, 112, 131 and 136. Field
names for 66–199 come from insta360-rs, GyroView or orbis, or are unknown.

## 4. Lens calibration strings

### 4.1 Grammar

Tokens are decimal numbers joined by `_`. Token 0 is the lens count *N* (2). The last token is a
**packed word**. In between come *N* per-lens blocks, plus, for V1 only, a global
`canvas_w canvas_h` before the word. [bytes] (Pat's four strings, "Pat's X5 samples": each
starts with `2`, and V1 ends `…_10752_5376_1137`)

Packed word [bytes] (Pat's four words: V1 `0x471`, V2 `0x20400`, V3 `0x30400`, V6 `0x60400`):

- `word >> 16` is the generation: 0 → V1, 2 → V2, 3 → V3, 6 → V6.
- `(word >> 8) & 0xff` is a body code: 4 on the X5. 11 on the X3 (`0x30B00`) and 12 on the ONE X2
  (`0x30C00`) are [unverified]: no X3 or X2 string has been read here. What any other code means
  is **unsettled**, and code 5 has no meaning here: the one given for it before came from
  telemetry-parser's rig, which M3's plan (4.1 rule 7) bars, and is not to be used.
- In V1, `word & 0xff` is the lens type: Pat's `0x471` gives 113, the `type` that every block of
  Pat's V2, V3 and V6 ends with. Whether higher bits belong to it, the bytes can't say.

| Gen | Field | Tokens | Per-lens block |
|---|---|---|---|
| V1 | 5 (17) | 6N + 4 (16) | `radius cx cy yaw pitch roll` |
| V2 | 53 (55) | 16N + 2 (34) | `radius cx cy yaw pitch roll tx ty tz c0 c1 c2 c3 w h type` |
| V3 | 54 (56) | 19N + 2 (40) | `xi fx fy cx cy yaw pitch roll tx ty tz k1 k2 k3 p1 p2 w h type` |
| V6 | 111 (112) | 27N + 2 (56) | `xi fx fy cx cy yaw pitch roll tx ty tz d0…d12 w h type` |

Sources for the table: [bytes], Pat's four strings ("Pat's X5 samples"), which settle these and
no more:

- **The counts**: 16, 34, 40 and 56 tokens, which are 6N + 4, 16N + 2, 19N + 2 and 27N + 2 for
  N = 2.
- **Where the blocks end**: each block of V2, V3 and V6 ends in `10752 5376 113` (V2's tokens 14
  to 16 and 30 to 32, V3's 17 to 19 and 36 to 38, V6's 25 to 27 and 52 to 54), and V1's two
  blocks of six are followed by `10752 5376` and the word.
- **Where the centre and the angles sit**: V1's tokens 2 to 6, `2701.130 2692.130 -0.198 0.211
  90.280`, recur as V2's tokens 2 to 6 and V3's 4 to 8, and lens 1's the same way (V1's 8 to 12,
  V2's 18 to 22, V3's 23 to 27).
- **That the centre is on the whole canvas**: lens 1's `cx` is 8069.000 in V1 to V3 and
  8075.730 in V6, that is 5376 plus a centre near the middle of its own square (2693.000 and
  2699.730), as lens 0's is of its own (2701.130 and 2713.050).
- **Where the translation sits**: the three tokens after the angles, all 0 for lens 0, and lens
  1's third about −0.032 in V2, V3 and V6.

The names in the blocks are **labels**, and the bytes don't settle all of them. They don't say
which of the three angles is the yaw, the pitch or the roll (the third is about 90 for both
lenses, §4.5), that V3's five terms are k1 k2 k3 then p1 p2 (the order §4.3's V3 model takes; on
Pat's files the last two are the small ones), or what V2's c0 to c3 and V6's d0 to d12 are
(§4.3). These stay **unsettled**: M3's plan reads them through generic spaces (6.1, 6.2), and
Spherecut's parser keeps the terms in token order. Units are pixels of the calibration canvas,
degrees and metres, which the sizes of Pat's numbers fit (lens 1's tz, 32 mm, is about the
lenses' spacing, §4.2). `w h` is the **whole side-by-side canvas**, not one lens, and lens 1's
`cx` includes the width of lens 0's area. [bytes] (X5: canvas 10752×5376; lens 1 `cx` =
8069.000 = 5376 + 2693.000 on Pat's V3, and 8082.1 = 5376 + 2706.1 on the GyroView fixture)

Which model writes which strings:

- X5: V1, V2, V3 and V6 [bytes].
- X6: V6 only [reported] `OR insta360-x6.md`.
- ONE X2: V1, V2 and V3, no V6 [reported].
- X3: V3, with V1 shown by ExifTool [reported] `RF`.
- ONE RS: V1, V2 and V3 [reported] (`GV adr/0009`: lens poses agree within 0.15° between them).
- X4: whether it writes V6 is [unverified] (Q3).

Canvases:

| Model | Canvas | Per-lens area | Lens type | Source |
|---|---|---|---|---|
| ONE X2 | 6080×3040 | 3040² | 41 | [reported] `OR` |
| X3 | 11904×5952 | 5952² | 71 | [reported] `RF` (real V3 string, fw v1.0.93) |
| X4 | **16000×6000** | **8000×6000 (not square)** | ? | [reported] `RF`; [code] `TP` commit 98b8e7b comment: "X4: cx = 3987.5, w = 1920, lens_width = 16000.0, cy = 3012.7, h = 1920, lens_height = 6000.0" |
| X5 | 10752×5376 | 5376² | 113 | [bytes] |
| X6 | 15488×7744 | 7744² | 193 | [reported] `OR insta360-x6.md` |

### 4.2 Example: X5 office clip, lens 0 [bytes]

```
V3: xi 2.000000  fx 4296.660 fy 4295.450  cx 2689.890 cy 2681.940
    ypr -0.002 0.377 90.524  t 0 0 0
    k1 0.18113680 k2 2.16784811 k3 -3.49636626  p1 -0.00016818 p2 -0.00010206
    10752 5376 113            (lens 1: cx 8082.100, t = -0.000907 -0.000055 -0.032061)
V6: xi 2.000000  fx 4291.290 fy 4290.310  cx 2688.740 cy 2680.400
    ypr -0.007 0.396 90.543  t 0 0 0
    d0..d12: 0.25187662 1.30721676 0.37999845 -5.84353542 0.00000000
             -0.00043939 -0.00052634 -0.00503890 0.00351397
             0.00160383 0.00053353 0.01195207 -0.00420138
    10752 5376 113            (lens 1: t = -0.000134 0.000012 -0.031739)
```

The fifth V6 coefficient (token 15) is exactly 0 for both lenses on both clips. [bytes] The
"original" copies (17, 55, 56, 112) equal the working strings on both fixtures. [bytes]
Lens 1 sits about 32 mm behind lens 0 along `tz` on the X5 [bytes], about 21 mm on the X2, and
about 27 mm on the X3 [reported].

### 4.3 Projection models

**V3: Mei unified model with Brown distortion.** For a ray (x, y, z):

```
n  = |(x,y,z)|
mx = (x/n) / (z/n + xi),  my = (y/n) / (z/n + xi)
r2 = mx² + my²,  R = 1 + k1 r2 + k2 r2² + k3 r2³
xd = mx R + 2 p1 mx my + p2 (r2 + 2 mx²)
yd = my R + 2 p2 mx my + p1 (r2 + 2 my²)
u  = fx xd + cx,  v = fy yd + cy
```

[code] `GF src/core/stabilization/distortion_models/insta360.rs` (GPL-3.0-or-later; its
`k = [k1, k2, k3, p1, p2, xi]`), the same in `video-types` `geometry.rs` `BrownV3`. The sphere
folds at θ = acos(−1/ξ): **120° off-axis for ξ = 2**, beyond the 100° half-field the X5 lens
images. Gyroflow inverts it with a bracketed Newton iteration. [code] raw360-formats checked V3
plus lens pose against Insta360 Studio exports of X3 photos, to within about 1° of yaw.
[reported] `RF`

**V6: Mei with 13 terms.** [conflict] on what the 13 are.

- `video-types` `RadtanPro` [code] [RE], **Studio-derived**: video-types names this reading
  after a vendor-internal class (`RadtanDistortPro`), so like telemetry-parser's rig (§8) it is
  a statement of vendor internals, and Spherecut takes nothing from it (M3's plan, 4.1 rule 7,
  which reads d5 to d12 through a generic space instead). It reads:
  `R = 1 + d0 r² + d1 r⁴ + d2 r⁶ + d3 r⁸ + d4 r¹⁰`, then
  `xd = mx R + (r²+2mx²)(d5 + d7 r²) + 2 mx my (d6 + d8 r²) + d9 r² + d11 r⁴` and
  `yd = my R + (r²+2my²)(d6 + d8 r²) + 2 mx my (d5 + d7 r²) + d10 r² + d12 r⁴`. That is five
  radial terms, tangential terms with an r² extension, and thin prism. Its comment says the
  tangential pair is stored "in opposite order to BrownV3".
- GyroView and raw360-formats read it as `k1..k5, p1, p2, p3, p4, s1..s4` and draw only
  `k1..k5, p1, p2`. [reported] `GV adr/0032` (provisional), `RF`
- orbis reads it as `k1..k4, reserved, p1, p2, s1..s4, reserved×2`. [reported]
  `OR insta360-x5.md §5.3`
- The coefficient at index 4 (`d4` / `k5` / "reserved") is 0 on both X5 clips [bytes], so the
  readings agree there. They differ in the tangential order and in the last six terms.
  Q5 resolves this.

**V2: odd-free polynomial in θ.** `r = θ(c0 + c1θ + c2θ² + c3θ³)` (normalised, c0 = 1 on the
X5), with focal `f = radius / r(θ_max)`. θ_max is the lens class's half-field: 200° FOV for most
bare lenses, 195° or 190° with accessories. [code] `TP rig.rs:500-540`. Field 145 gives the same
fields of view on the X5 [bytes] (§3.5). The θ_max table behind them in that file is [RE] and
**is not to be used**: M3's plan (4.1 rule 7) bars telemetry-parser's rig and its V2 θ_max
table (§8).

**V1: equidistant fisheye.** Only `radius` and a centre are given. The radius marks about 100°
off axis. [reported] `RF` Only a few legacy lens types get Studio's built-in polynomial. [code]
[RE] `TP rig.rs:500-520`

### 4.4 From canvas pixels to video pixels [conflict]

The video track (X5 2880² or 3840²) is smaller than the calibration area (5376²), and field 27
(`5376 → 5312`) describes a sensor window. Three readings exist.

| Reading | Focal | Centre | Who |
|---|---|---|---|
| A | `f · side / dst` (5312) | `c_local · side / src` (5376) | telemetry-parser's single-lens profile [code] `TP mod.rs:284-309`; orbis X5 and X2 (on the X2 `fx · 2880/2880` and `cx · 2880/3040`; stitch checked by seam correlation) [reported] |
| B | `f · side / dst` | `(c_local − (src − dst)/2) · side / dst` (a centred crop window) | raw360-formats, measured on an X3 proxy, an X3 pair and an X4 file [reported] |
| C | `f · side / src` | `c_local · side / src` (uniform: "the frame shows the whole calibration square") | telemetry-parser's rig [code] `TP rig.rs:454`; GyroView [reported] `GV adr/0014` |

A and B give the same centre to within a pixel when the principal point is near the middle,
but A and B differ from C by `src/dst` in focal length: **1.2% on the X5** (5376/5312) and
**5.5% on the X2** (3040/2880). GyroView also reports a further per-unit radial scale of 0.8%
to 4.5% on the X5 and X6, contested on a second unit. [reported] `GV adr/0023`

The **X4's 8000×6000 per-lens area** breaks every square-canvas assumption. telemetry-parser
scales `cx` by `1920/8000` and `cy` by `1920/6000` (i.e. non-uniformly) [code]. That only works
if the square video is a crop of a 4:3 sensor area. This is unverified (Q3).

### 4.5 Extrinsics

- `yaw pitch roll` are in degrees. Roll is about 90° on X5 and X3 (how the sensors are
  mounted); on the X2 it is about 179° for lens 0 and 1.4° for lens 1, and the X2 lens pictures
  lie on their side. [bytes] [reported]
- The strings do **not** include the 180° back-to-back turn between the lenses; every reader
  adds it. [code] `TP rig.rs:312-330`
- The Euler composition is [conflict]:
  - telemetry-parser's rig uses `Ry(pitch + 90°) · Rz(yaw) · Rx(roll)`, mapping world to
    camera, with lens 0 right-multiplied by `Rz(π)` [code] [RE].
  - telemetry-parser's older IMU rotation uses `Rz(roll) · Ry(pitch) · Rx(yaw)` [code]
    `TP mod.rs:328-345`.
  - insta360-rs uses `Rz(ez)·Ry(ey+90°)·Rx(ex)`, with lens 1 = `Ry(180°)·R` [unverified].
  - GyroView fitted its convention against Studio, including roll "mirrored about the nearest
    quarter turn" [reported] `GV adr/0025`.
  - orbis uses "the stored roll less 90°" [reported].
  Spherecut needs its own measurement (Q6).
- `tx ty tz` are in metres and real. telemetry-parser says zeroing them throws away a
  13-pixel parallax correction at infinity on an X5, and that the best stitching distance
  depends on the scene (about 0.8 m on its clips). [code] `TP rig.rs:12-40`

### 4.6 Lens types and accessories

The lens-type token (X5 113, X3 71, X2 41, X6 193) distinguishes the bare lens from guards
and dive cases. telemetry-parser's tables of types per accessory (`rig_body_masks.rs`,
`rig_mask_coefficients.rs`) are generated from Insta360 Studio 6.0.2 disassembly [RE], and
Spherecut must not copy them. The clean-room sources for accessory state are field 145's
per-accessory FOV and polynomial [bytes] and field 68 [unverified]. Field 13
`user_options.offset_convert_states` (WaterProof, DivingWater, DivingAir, …) [code] may say
whether the strings were already converted for an accessory. [unverified]

## 5. IMU axes and gyro/video sync

### 5.1 Axis conventions

telemetry-parser emits an **orientation string** per model. Each letter names the source axis
for output x, y, z, and lowercase negates it: `"yzX"` means out = (−y, −z, +x). [code]
`TP tags_impl.rs:253-263` The table depends on whether `offset_v3` has at least 20 tokens.
[code] `TP mod.rs:178-202`

| Model string | With V3 | Without V3 |
|---|---|---|
| Insta360 X4, X5 | `yzX` | (`yXZ` default) |
| Insta360 OneR / OneRS | `Xyz` | `yXZ` / `yxz` |
| Insta360 ONE X2 | (`Xyz` default) | `xZy` |
| GO 2 / GO 3 / GO 3S / GO Ultra | `XYZ` / `XYZ` / `yXZ` / `YxZ` | `yXZ` (GO 2), `xyZ` (Go) |
| anything else | `Xyz` | `yXZ` |

With V3 present, telemetry-parser **first rotates the raw gyro and accelerometer by lens 0's
yaw, pitch and roll** (about 90° of roll on most bodies), and the strings above assume that
rotation. [code] `TP mod.rs:327-372` (commit bf8924f, 2026-10-14). Because the ONE X2 has V3,
the `xZy` entry is never used for it; it gets `Xyz` after the rotation. [reported]
`OR insta360-one-x2.md §8`

Independent measurements, each in its own body frame:

| Model | GyroView body (x right, y down, z = lens 0) from IMU | orbis sphere (+X right, +Y up, −Z forward) from IMU | Status |
|---|---|---|---|
| X5 | (x, z, −y) | `z, −x, −y` | both [reported], measured from motion on several clips |
| X6, X4 Air, X3 | (x, z, −y) | | [reported] one recording each (X3 by levelling against Studio) |
| ONE X2 | | `z, x, y` (the X5 map turned 180° about X) | [reported] |
| ONE R 360 module | (x, −z, y) | | [reported] one recording |
| ONE RS 1-inch 360 | (−y, x, z) **assumed** | | [unverified]. GyroView: Gyroflow's ONE RS and X3 entries "put their down on another axis than the one measured" |
| Antigravity A1 | (−z, −x, y) | | [reported] |

The frames are defined differently: forward lens, handedness, and which track is "lens 0"
(§2.6). These tables therefore cannot be compared entry by entry without a sample. Clip 10 of
`docs/SAMPLES.md` exists to settle this per camera. The accelerometer shares the gyro's axes
[reported] `OR`, `GV adr/0009`.

### 5.2 Time base

What the bytes show for the X5 office fixture [bytes]:

- `first_frame_timestamp` (field 24) = 921 751 839 µs, and it **equals exposure entry 6
  exactly**. The exposure record begins six frames (100.1 ms) before the first video frame,
  which is why it holds 15 720 entries for a 15 711-frame clip. orbis says the first frame
  equals an exposure timestamp exactly. [reported]
- The IMU starts at 921 648 752 µs, **103 ms before the first frame**. orbis reports X5
  pre-roll of 157–778 ms; on the ONE X2 the IMU starts about 1.6 s early. [reported]
- Field 28 = 1.6 ms, field 29 = 1.

telemetry-parser normalises every IMU and exposure sample to **seconds relative to the first
frame** [code] `TP mod.rs:233-266`:

```
t = record_ts/1000 − field24/1000         (ms-like units)
if is_raw_gyro: t = t/1000                 (raw clocks are µs)
gyro/accel only: t −= field28/1000         (only when field 29 is set)
```

Gyroflow then gives every frame a time offset [code] `GF src/core/gyro_source/mod.rs:591-622`:
`offset_ms = −(exposure_s·1000/2) − 0.9 − (video_ts − exposure_t)·1000 − timemap(t−v) −
zero_ref`. This puts frames at **mid-exposure** and includes a constant whose comment reads
"The additional 0.9 ms is a mystery". Exposure entries before the first frame have negative
`t` and are skipped, which fits the pre-roll above.

orbis fitted a different model on X5 clips [reported] `OR insta360-x5.md §7.1`:

```
gyro time of video t (lens 0) = t·(1 + rate) + (field24 − imu[0].ts)·1e-6
                               + field28·1e-3 + timeMap·1e-3·(1 + rate) − 7.3e-3
lens 1 captured at            + field25 / 3
```

It reports a **frame-clock drift of 15–40 ppm** against the IMU clock, and finds that the two
lenses are not exposed at the same instant. GyroView also times frames at mid-exposure.
[reported] `GV adr/0034`

Rolling shutter: field 25 is the readout time per clip, in ms. It varies by mode (X5 8.4 ms at
5.7K60, 21.2 ms at 8K30), so use the file's value, not a constant. [bytes] [reported]

Proposed for Spherecut:

- Convert every timestamp to seconds on the camera clock.
- Anchor video frame *k* to exposure entry `k + i0`, where `i0` is the index of the entry equal
  to field 24 (fall back to `field24 + k/fps`).
- Use mid-exposure plus a per-row readout from field 25.
- Apply field 28 to the IMU.
- Expose one residual offset and a drift (ppm) to be fitted by optical flow.

Do not hard-code 0.9 ms or −7.3 ms until our own samples (clips 9 and 10) say so.

## 6. LRV proxies

| Model | Name | Video | Layout | Trailer | Source |
|---|---|---|---|---|---|
| ONE X2 | `LRV_…_11_NNN.insv` | H.264 Main 736×368, 25 fps, about 14 Mb/s, GOP 15 | packed 2:1, **left half = `_00_`**, one frame shorter than `_00_` | full; IMU, exposure and record 9 are byte-identical to `_00_`; metadata differs in field 9 and `file_group_info.index` = 1 | [reported] `OR insta360-one-x2.md §3.2` |
| X3 and ONE X | `LRV_…_{01,11}_…` | ? | packed 2:1 presumably | ? | [vendor] names only |
| X4, X4 Air | `LRV_…_01_….lrv` | ? | ? | ? | [unverified] |
| X5 | `LRV_…_01_….lrv` | H.264 High **1664×832, 25 fps for a 25 fps master and 29.97 fps otherwise** (60 and 120 fps masters too, with half and a quarter of their frames), about 8 Mb/s [bytes]; "always 29.97 fps" and about 4 Mb/s [reported]; B-frames [reported]; a keyframe every 1 s, every 30 frames at 29.97 and 25 at 25 fps [bytes] (P-TRK, M3.0; "GOP 30" [reported]); full-range BT.709 [bytes] | packed 2:1, **left half = `.insv` track 1, right half = track 0** [bytes] | full: every record but the metadata byte-identical to the master's [bytes] | [bytes] Pat's X5 samples; [reported] `OR insta360-x5.md §3.2`, `GV FORMAT.md` |
| X6 | `.lrv` | 2048×1024, 8-bit HLG | packed | full | [reported] `GV FORMAT.md` |
| A1 | `.prx` | | | metadata and thumbnail only, no IMU | [code] commit message of `TP` 5051b45 |

telemetry-parser accepts `.lrv`. Its commit 5051b45 (2026-10-19) says 126 X5 and X6 LRVs had
metadata and calibration equal to their `.insv`, and IMU "identical sample for sample".
[code/reported] An LRV can be one or two frames shorter than its master, and its own metadata
gives its own frame count. [reported] `OR`

Use for Spherecut: LRVs make ready-made proxies for the timeline, at least on the X5, since
they share the trailer. Their side-by-side halves need their own lens order per model (on the
X5, left = track 1 [bytes]), and the mapping from proxy pixels to calibration canvas is
[unverified]. They do not exist in several older-camera modes (§2.2).

## 7. Codecs, bit depth, colour, quirks

| Model | Codec (vendor) | Seen in files | Per-lens sizes | Bit depth, range, colour | Max bitrate (vendor) |
|---|---|---|---|---|---|
| ONE X | H.264 | | 5.7K total 5760×2880 | ? | 100 Mb/s |
| ONE X2 | H.264, H.265 | H.264 Main L5.2, `avc1`, about 45 Mb/s, GOP 16, no B-frames | 2880² (5.7K) | 8-bit `yuvj420p` full range, **tagged BT.601** (`smpte170m`) [reported] | 100 Mb/s |
| X3 | H.264 | | 5.7K split, 4K/3K packed | ? (LOG offered) | 120 Mb/s |
| ONE RS 1-inch 360 | H.264 | H.264 [reported] | 3072² at 24 fps [reported]; vendor lists 6K 6144×3072@25/24, 5888×2944@30, 4K 3840×1920, 3K 3040×1520@50 | ? | 120 Mb/s |
| X4 | H.264, H.265 | `hvc1` whose **hvcC profile, tier and level bytes are zero** [reported] `RF` | 3840² (8K), 2880² (5.7K), 1920² (4K) [unverified] | **unknown**, including 10-bit | 200 Mb/s |
| X4 Air | H.264, H.265 | `hvc1` 8-bit [reported, one recording] | ? | ? | 180 Mb/s |
| X5 | H.264, H.265 | HEVC Main `hvc1`, about 94 Mb/s at 8K30, GOP 60, no B-frames; a keyframe every 2 s at 29.97, 25 and 59.94 fps and every 1 s at 119.88, and `ctts` all 0, in both tracks [bytes] (P-TRK, M3.0, §2.5) | 3840² (8K30), 2880² (5.7K, 5.7K+, 5.7K60), 1920² (4K120) | **8-bit `yuvj420p` full range, BT.709, in every mode including I-Log** [reported] `OR` | 180 Mb/s |
| X6 | H.265 | HEVC **Main 10** in every mode | 3840² (8K, 50 fps), 3008² (6K), 1920² (4K) | Standard and I-Log tagged BT.709; **Dolby Vision is HLG BT.2020 limited range with no RPU**; colour in the SPS only, no `colr` box [reported] `OR insta360-x6.md`, `GV FORMAT.md` | 180 Mb/s |
| A1 | | HEVC `hvc1` with a **blank hvcC** | 3840² | 8-bit full range BT.709 [reported] | |

Vendor codecs, bitrates and colour profiles are from `IM shooting-specs` [vendor]: "Standard,
Vivid, and Flat" for X5, X4 and X4 Air; "Standard, Vivid, and LOG" for X3 and X2; "Standard,
Dolby Vision, and I-Log 10-bit" for X6. That page lists no I-Log for the X5, yet orbis has X5
I-Log clips with `gamma_mode` `I_Log` [reported]. Per-model resolutions on the spec page did
not survive extraction.

Quirks to handle:

- **Full range.** Most bodies flag `yuvj420p` / `color_range=pc`. Treating it as limited range
  lifts blacks and crushes whites. [reported] `OR insta360-x5.md §3.1`
- **Read the colour tags from the stream.** The X2 says BT.601, the X5 BT.709 and the X6
  BT.2020/HLG, and the X6 says so only in the SPS. [reported]
- **Read the HEVC profile from the SPS when hvcC is zeroed** (X4, A1). [reported] `GV adr/0037`,
  `RF`
- **Log signalling** is only in `gamma_mode` (field 22), not in VUI. [reported] `OR`
- **Rotation**: the sources disagree on how rotation is signalled, and none mentions `tkhd`
  matrices. Candidates are field 46 `cam_posture` (0–3), field 52 (EXIF-style codes) and field
  130 (1 = 0°). Gyroflow's UI turns the preview by `cam_posture` (with a GO 3S special case) [code] `GF src/ui/VideoArea.qml:257-260`.
  GyroView ignores all three and derives the mounting from gravity [reported] `GV adr/0038`.
  Whether the `tkhd` matrix is ever non-identity is unknown (Q10).
- **Fisheye circle fill**: on the X5 the circle touches the frame edge (radius about 1920 px in
  3840²); on the X2 it does not (about 1403 px in 2880²). [reported]
- The camera's own reframe and FOV defaults appear in fields 47–49. [bytes, meaning
  unverified]

## 8. telemetry-parser: licence, crate and provenance

**Licence.** `MIT OR Apache-2.0` (`TP Cargo.toml`). Every file I read carries
`SPDX-License-Identifier: MIT OR Apache-2.0`. That is compatible with distributing Spherecut
under GPL-3.0-or-later; both are already in `deny.toml`'s allow list.

**crates.io.** The crate `telemetry-parser` has only **0.2.5 and 0.2.6**, both published
2022-09-06 with edition 2021 (checked through the crates.io API today). Those predate the X4,
X5, X6, A1 and LRV support, the indexed trailer and the V6 strings. The repository is at
**0.4.0** (edition 2024, no `rust-version`), and its last tag is v0.3.0 from 2024-01-16.
Gyroflow itself depends on it by git:
`telemetry-parser = { git = "https://github.com/AdrianEddy/telemetry-parser.git", rev = "2817b42" }`
(`GF src/core/Cargo.toml:14`).

**Dependencies at 2817b42.** These include `mp4parse-patched` 0.17.1 (MPL-2.0),
`fc-blackbox-patched` 0.2.1, `video-types` 0.1 (MIT/Apache; 0.1.2 was published
**2026-10-22, today**, with rust-version 1.88), `prost` 0.14, `serde_yaml` 0.9 (archived
upstream), `chrono`, `csv`, `half`, `memchr` and `argh`. `cargo metadata` resolved 71 packages,
and every licence matched `deny.toml`'s allow list (my check, scratchpad). None of the banned
crates appear. `build.rs` is a no-op. Whether it builds with Spherecut's MSRV 1.88 is
**[unverified]** (edition 2024 needs at least 1.85, and video-types needs 1.88), and so is
cargo-audit's view of the tree.

**Using it would collide with `deny.toml`**, which sets `[sources] unknown-git = "deny"` and
allows only the crates.io registry. A git dependency needs `allow-git =
["https://github.com/AdrianEddy/telemetry-parser"]` and a DECISIONS.md entry, as `deny.toml`'s
own comment requires. The alternative is to vendor a pinned copy as a path crate.

**Provenance.** This matters more than the licence.

- `src/insta360/rig.rs`, `rig_body_masks.rs` and `rig_mask_coefficients.rs` (©2026) state that
  their constants come from **disassembled Insta360 Studio 6.0.2**. Examples: "Studio 6.0.2
  arm64: Lens::getCoeff 0x4c5e48", `createStaticDewarpMap @ 0x17F54E4`, `GetFov 0x17f3d70`.
  The generator script lives in a non-public part of oxivideo; the public oxivideo repository,
  branch `legacy`, has no `docs/360/oracle`.
- The protobuf enum names were "read out of [the vendor's] embedded schema" (`TP rig.rs:610`).
- **video-types**, a telemetry-parser dependency, states vendor internals too: its
  `RadtanDistortPro` name and reading of V6 (§4.3), and a mask angle and blend width it gives
  for Insta360's lenses (0.1.2 `src/geometry.rs:1267, 1451-1452, 1622-1630`; their values are
  left out here on purpose). M3's plan (4.1 rule 7) keeps them out of Spherecut, as it does the
  rig files above.
- PROMPT.md allows "reverse engineering my own sample files". Disassembling the vendor's
  application is a different act, and Studio's EULA terms are unknown to me. **This is a
  decision for the user, not legal advice.**

**insta360-rs** (OpenSpeleo, labelled Apache-2.0) bundles about 49 MiB of files copied from
Insta360's licensed MediaSDK 3.1.5, CameraSDK 2.1.8, iOS SDK and Studio 5.9.10, by its own
`docs/sdk-provenance.md` and `NOTICE.md`. **Do not depend on it or copy from it.** Its format
notes appear above only where marked, and only as leads.

**Clean references.**

- orbis-reframe (GPL-3.0-or-later; its `src/insv/` is MIT OR Apache-2.0) has an explicit
  no-disassembly policy, and its documentation is built from the maintainers' own clips.
- GyroView (MIT) and raw360-formats (MIT, Dart) measure on their own files.
- ExifTool (Artistic or GPL) is file-derived.
- Gyroflow's `insta360.rs` distortion model is GPL-3.0-or-later. Its *maths* (the Mei/Brown
  model) is textbook in any case.

**Recommendation.** Write Spherecut's own `camera::insta360` reader from this document.
The footer and index are about 100 lines; the record decoders a few hundred; the protobuf can be
a hand-rolled varint walker over the fields we need, or `prost` with our own minimal `.proto`
naming only the field numbers used. Build our own geometry from the strings and our own
measurements. Keep telemetry-parser out of the shipped graph. Use it as a **differential-test
oracle** in a separate, non-workspace tool or in xtask behind a feature, so it never enters
`cargo deny`'s product graph, and compare IMU samples, timestamps and string parses on every
sample clip. (M3: not used, not even as an oracle, for the X5's trailer or DJI's calibration,
by `docs/design/m3-plan.md` §20 and the plan's D62 (§23), which DECISIONS.md records at M3.21.
M4 still decides whether it parses the IMU.) If the user
also wants it for GoPro GPMF or DJI telemetry, reconsider a pinned
git dependency (`rev = "2817b42"`) that uses only its parsing output (`Lens/Data` and the IMU
tags) and never its `Lens/Rig`. That needs `allow-git`, a DECISIONS.md entry, and the user's
sign-off on the provenance point above.

## 9. Open questions that need real samples

The clip numbers refer to `docs/SAMPLES.md`.

| # | Question | Clips that answer it |
|---|---|---|
| Q1 | Which Insta360 camera(s) the user owns, firmware versions, and whether those match the table in §2.2 | NOTES.txt |
| Q2 | **Track ↔ lens binding** on one-file cameras (field 131 = 4, 80 = 1 → track 0 = screen-side lens?). The X5's LRV half order is answered by the bytes (left = track 1, Pat's X5 samples). *Suggestion: add "cover the screen-side lens with a hand for 2 s at the start" to clip 8.* | 8, 1 |
| Q3 | **X4**: canvas 16000×6000 against the square tracks, field 27 values, V6 presence, `is_raw_gyro`, field 65, index alignment, hvcC, bit depth in each codec | 1, 3, 4, 5 |
| Q4 | Canvas → video focal scaling: reading A/B (`dst`) or C (`src`), 1.2% on the X5, and any per-unit radial scale | 8 (with straight lines across the seams), 12 (Studio export) |
| Q5 | Meaning and order of the V6 coefficients (the readings of §4.3: k1..k5 p1..p4 s1..s4, orbis's, or video-types' `RadtanPro`, which is Studio-derived [RE] and not used) | 8, 12 |
| Q6 | Euler composition and sign conventions for `yaw pitch roll`, and the sign of `tz` | 8, 12 |
| Q7 | IMU axis map per model, including the X4 (only telemetry-parser's `yzX` so far) and the ONE RS (assumed) | 10, then 9 |
| Q8 | Sync constant (Gyroflow 0.9 ms, orbis −7.3 ms plus a lens-1 delay of readout/3), drift in ppm, and field 28's sign | 10, 9 |
| Q9 | Whether FlowState or in-camera stabilisation alters the recorded pixels or only metadata (field 42), and whether accessory modes rewrite `offset` against `original_offset` | 11 |
| Q10 | Rotation signalling: `cam_posture` (46), field 52, field 130 and the `tkhd` matrix when the camera is mounted upside down or sideways | 9 (flips); an upside-down clip is not in SAMPLES.md |
| Q11 | Log and 10-bit: `gamma_mode` values, VUI and `colr` tags, whether X4 or X5 ever write Main 10 | 5 |
| Q12 | Split recordings: file naming of the chapters, field 26 contents, whether the IMU and exposure clocks continue across files | 13 |
| Q13 | 24 and 25 fps timebases, whether `_10_` is one frame shorter on every two-file camera, and whether `_10_` lacks a trailer on the ONE RS 1-inch | 2, 1 |
| Q14 | File layout of the 60 fps and highest-fps modes (one file on X3 below 5.7K?), and the missing LRV | 3, 4 |
| Q15 | Spatial audio: channel count and layout of the AAC streams, field 61 `audio_mode` (9 on X5), and the two-stream case with an external mic | 6, 7 |
| Q16 | Thumbnail records (2, 5) per model: an H.264 IDR frame or an NV12 equirect | any |
| Q17 | Records 11, 22, 27, 28, 29 (several MB on the X5): content unknown; probably not needed | any |
| Q18 | App-trimmed or transferred clips: records 0x39 and 0x80, and whether app transfer drops the `_10_` file or the trailer | optional: one app-trimmed copy of clip 1 |

## Sources

All repositories were cloned from GitHub at the stated commit on 2026-10-22. Paths are relative
to each repository root.

| Key | Repository / URL | Commit, date | Licence | Notes |
|---|---|---|---|---|
| TP | https://github.com/AdrianEddy/telemetry-parser | 2817b42, 2026-10-22 | MIT OR Apache-2.0 | `src/insta360/{mod,record,extra_info,rig,rig_body_masks,rig_mask_coefficients}.rs`, `src/tags_impl.rs`, `src/util.rs`; commits 98b8e7b, bf8924f, 5051b45, 54c85e8 |
| GF | https://github.com/gyroflow/gyroflow | 1a78c95, 2026-10-22 | GPL-3.0-or-later | `src/core/stabilization/distortion_models/insta360.rs`, `src/core/gyro_source/mod.rs:591-622`, `src/util.rs:568-600` (`is_insta360`, `copy_insta360_metadata`), `src/core/Cargo.toml:14` |
| LP | https://github.com/gyroflow/lens_profiles | 886fc61, 2026-07-28 | CC0 | 142 community Insta360 profiles, OpenCV fisheye, mostly flat or reframed modes; not usable for raw dual fisheye |
| ET | https://github.com/exiftool/exiftool | 2200871, 2026-05-27 | Artistic / GPL | `lib/Image/ExifTool/QuickTimeStream.pl` (`ProcessInsta360`, `%insvDataLen`), `QuickTime.pm` (`inst` box) |
| OR | https://github.com/Schabo/orbis-reframe | 70a9ba2, 2026-10-22 | GPL-3.0-or-later (`src/insv` MIT OR Apache-2.0) | `docs/formats/insta360-{x5,one-x2,x6,x4,x3}.md`, `antigravity-a1.md`; X5 and X2 from own clips, X4 and X3 desk research |
| GV | https://github.com/bubo-squared/GyroView | 00f1f74, 2026-10-21 | MIT | `docs/FORMAT.md`, `docs/adr/0009, 0014, 0023, 0025, 0032, 0034, 0037, 0038`, `test/fixtures/x5/` (the bytes decoded here) |
| RF | https://github.com/freeKC/raw360-formats | fc660ec, 2026-10-21 | MIT | `docs/insta360.md` (X3 and X4 observations) |
| IR | https://github.com/OpenSpeleo/insta360-rs | 7dd9b3e, 2026-10-06 | Apache-2.0 (bundles vendor SDK files) | `docs/INSV_FORMAT.md`; leads only, see §8 |
| MM | https://github.com/gyroflow/mp4-merge | 519ed0d, 2025-06-02 | MIT OR Apache-2.0 | `src/insta360.rs`: merges trailers of split files (concatenates binary records except ids 0, 1, 2 and 5, and rewrites a sequential footer) |
| IV | https://github.com/alex-plekhanov/insvtools | 0f8ebe3, 2024-09-28 | Apache-2.0 | trailer version check, LRV name example |
| VT | `video-types` 0.1.2 on crates.io (repository AdrianEddy/wgpu-interop) | published 2026-10-22 | MIT OR Apache-2.0 | `geometry.rs`: Mei, BrownV3, RadtanPro |
| IM fileformat | https://onlinemanual.insta360.com/x5/en-us/operating-tutorials/storage/fileformat | fetched 2026-10-22 | vendor | file counts per model and mode, naming |
| IM shooting-specs | https://onlinemanual.insta360.com/x5/en-us/specs/shooting-specs | fetched 2026-10-22 | vendor | codecs, bitrates, colour profiles per model |
| IM compare | https://www.insta360.com/product/compare/x4-vs-oners_1inch_360 | fetched earlier in this research | vendor | ONE RS 1-inch 360 resolutions and bitrate |

I also read two repositories that are not cited for any claim, judging them unreliable or
stale. BenjaminHenriksson/insv-stitch (722b9c7) describes X5 IMU as gyro first, 200 Hz, in
m/s², which the bytes contradict. EvertSchippers/InstaTrailer.jl (2020) is ExifTool-derived.
