# Spherecut M3 (Raw ingest and stitching): the implementation plan

*2026-11-01, before M3's code, at main fde9540 (0c0296b's code plus odin's A2 report). Written from
PLAN.md M3, docs/PROMPT.md (Stitching, Inputs, Testing and the performance targets),
docs/formats/insta360.md, gopro.md and dji.md, docs/SAMPLES.md (takes 14 to 17, drafted
2026-11-01 and committed with this plan), DECISIONS.md D1 to D59, the code as M2 and A2 left it, and the
M3 map (`understand.md` and its six reports, which read all 14 of Pat's X5 files at firmware
v1.1.22). Three drafts (risk first, value first, verification first) were scored by three judges.
This plan starts from the verification draft and takes in what the judges named from the other
two, with every error they found fixed. Where the judges disagreed, the choice and its evidence
are given where the choice is made; Appendix B lists them. Tags: **[F]** checked on the code or
the bytes, **[G]** a guess or proposal, **[C]** computed for this plan from Pat's V3 and V6
numbers (section 3.4). Section 0 summarises a fresh review's findings; the sections below are
corrected to match, hold the detail, and win where the two differ.*

## 0. Amendments (read first)

1. **The fresh review (2026-11-01)** raised 58 findings in five areas, and a skeptic upheld every
   one (one blocker, 26 major, 31 minor). The sections below are corrected to match, so they need
   no reading against this list; Appendix C gives each finding and where its fix went. The
   largest changes:
   - **The lock (9), the blocker.** All or none on top of per-stream upload and eviction froze
     the view whenever one lens lagged. The group now runs the cache and the upload from a base
     time both lenses hold, catches up together, keeps a floor of four live slots, reads track
     timing through the backend whatever the decoder, counts mixed pairs by content as well as by
     tag, and is tested for liveness. A short lens ends the pair instead of refusing the lock.
   - **What the seam can settle (6.2, 8.4 to 8.6).** Some conventions are exact twins under a
     common quarter turn, and free depth per sector absorbs a relative tilt. The fit reports what
     its information matrix determines and judges only that: clips 8 and 10 settle the binding,
     the V6 reading, the gross class (the turn axis, the roll offset and the Euler class up to
     twins) and the relative roll; the fine class waits for take 14, which now has a
     second heading and the tripod's height for a ground model.
   - **The error budget (3.2, 3.4).** The V6 readings move a seam pixel 12 to 33 px a lens, mostly
     along the seam, so clips 8 and 10 can rank them; V3 against V6 matters on both lenses; a late
     lens under a turn is a rigid rotation, not "rest".
   - **Controls that can fail (4.2, 8.5, 10.5).** Rung 1 gains a common-mode control against the
     ideal, its feather and margin controls move to views where they bite, its band check compares
     against the lenses alone, and the fit's recovery suite expects the redirect verdicts too.
   - **The lens and synth (6.1, 6.3, 7.1).** The inverse iterates to its residual, the mask stops
     at the frame's edges, synth composes the extrinsics with its own code, and the V6 readings are
     a generic space with no vendor names (4.1 rule 7).
   - **The LRV (15)** opens as a two-member lock group over its one stream, each half cropped by a
     `Columns` view, and follows D25 on path B.
   - **Render (10).** `StillRenderer`'s `render_from`, `render_blurred` and `render_yuv` take `SourceFrames` (one frame or a pair), and `render`, `render_imported` and `render_copied` stay one-frame wrappers; the gains live beside the tune
     in core; the rig uniform gets its own group-0 bind group and the caps sit in group 2.
   - **huginn never blocks a slice.** M3.1 merges on odin's numbers, K4 covers huginn's 8K30
     export, and huginn's rows read "pending huginn" until they come.
   - **The exposure analysis and Estimate distance** stand aside while the viewer plays, as D56 has
     the export do, and the analysis is costed as the full decode it is.
   - **M3 can close as "done pending samples"** without takes 14 and 15 (M3.12), and the old
     question 4 is gone.
2. **A check of those fixes (2026-11-01).** Five checkers, one per review area, and a critic of
   the whole plan's consistency re-read the fixed plan against the code; skeptics kept 50 of the
   55 issues they raised (five were duplicates or refuted), and those are fixed in place too. The
   largest: wherever depth is free per sector, the tilt is undetermined by rule, since σ measures
   noise and a wall's depth lands on the tilt (8.4); `MediaBackend::track_timing` has no default
   body, so no wrapper backend refuses every lock unseen (9); the seek, catch-up and fallback
   cases of the lock's group base are spelt out (9); the V6 readings are named only by 6.1's
   fields everywhere; take 14's note gives the tripod height and that nothing stood nearer than
   10 m (24, SAMPLES.md).

## 1. Summary

M3 makes Pat's Insta360 X5 files usable without Insta360 Studio, then the GoPro MAX's EAC pair,
DJI's Osmo 360 (raw, or a written finding with its stitched export), and nadir and zenith caps.

What Pat does in the GUI:

1. Opens `VID_…_00_NNN.insv`. The app reads the trailer, builds the lens pair from the camera's
   own calibration, plays both lens tracks frame-locked, and stitches them in the projection
   pass. The raw lenses stay one toggle away (View ▸ Raw lenses).
2. Reframes, keys and exports exactly as in M2. The export reads the master, never the proxy.
3. Opens the Stitch pane when a seam shows. It shows the lens geometry with its status
   (provisional, measured, fitted or Pat's own) and where that came from. Pat tunes each lens (yaw,
   pitch, roll, centre, radius, field of view), the seam width and the stitch distance in metres,
   drags across the seam with Alt held to align it, and presses **Measure seam** to get a number
   (the misregistration in pixels and which term dominates) instead of judging by eye.
4. Saves the tune as a profile for this camera's serial; later clips from the camera open with it.
5. Plays the camera's `.lrv` as a proxy when 8K is too heavy, and gets lenses whose exposure and
   white balance match, in the preview and the export alike.

Headless, `spherecut probe` names the camera, its lenses and calibration, `spherecut frame
file.insv` renders a stitched still with no project, and `spherecut render` exports a project that
holds raw media. Agents get the same through `probe`, `project`, `edit` (`set_source`,
`set_stitch`, `set_caps`), `look` (`mode: "seam"`, with the seam's numbers) and `check`.

How the plan is built:

- **Geometry is data until it is measured.** Seven things about how the X5's calibration maps to
  its video are not known (3.2): which track is which calibration block, the focal scale from
  canvas to video, the Euler composition, the sign of the lens offset, V3 or V6, what V6's last
  eight terms mean, and which lens faces the screen side. Each is a field of `sphere::rig::Geometry`
  with a status and its evidence, folded by the CPU into one uniform, so settling one changes a
  table entry, never the shader, the goldens or the schema.
- **One instrument measures every seam.** The seam signature (section 8) splits the
  misregistration across the overlap into terms tied to parameters. Tier 1 proves that each
  planted error lands on its own term before any real footage is used. The same code scores the
  conventions on Pat's takes, under rules written before they arrive, and gives Pat and agents a
  number in the app and in `look`.
- **One round trip, grown rung by rung** (4.2). Each slice that adds a path (the GPU, the file, the
  export, the app, the proxy) adds a rung that must pass the rung below's checks on the same scene,
  rig and thresholds. Every check has a control that must fail.
- **Risks first, then pictures early.** The first slices measure what could invalidate a design:
  the tracks' timing tables (M3.0) and pair decode in the export (M3.1, on odin; huginn's numbers
  join whenever its answer comes). Stitched stills of Pat's clip 8 come with M3.6, under the
  geometry it fits on clips 8 to 10. Clip 8 plays stitched at M3.10 and exports at M3.11, the
  end-to-end point, with a note to Pat; neither is shown to Pat before M3.6's geometry is in.
  Takes 14 and 15 settle the geometry whenever they arrive (M3.12). Nothing else waits for them:
  without them M3 closes as "done pending samples".

## 2. What M3 builds on

- **sphere**: D33's axes (+X right, +Y up, −Z forward; `world = q · view`), `Ypr`, `grab`
  (`solve_free`); Kannala–Brandt fisheyes with a Newton inverse (`fisheye.rs:15-40`; a lens looks
  down its own −Z with +y up); EAC faces (`cube.rs:1-99`).
- **render**: `fs_project` (`project.wgsl:174-209`; it reads the source at 183-190,
  194-198, 203-205 and 207) and
  `fs_flat`, chosen by `Pass` (`project.rs:216-223, 515-519`); explicit layouts of three groups
  (view, source, sampler; `project.rs:527`); `LinearTarget` owns the view uniform and its group
  (303-386); four sampler bind groups (452); `draw` (651-685); `StillRenderer`'s one `slot`
  (`still.rs:179, 326-330, 465`); the CPU reference (`reference.rs:59, 203-216, 278-296,
  322-364`); whole-image `compare`; D32's 24 goldens. D31's stereo crop keeps the top-left part
  only, and nothing takes an origin: `Placement::shown` (`color.rs:96-101`) sizes it for the copy
  path (`Uploader::upload_placed`, `upload.rs:168-235`, rows read from each plane's first byte), for
  path B (`upload::shown_regions`, `upload.rs:639-652`, whose regions the Copier copies with no image
  offset, `vkshare.rs:795-809`) and for the CPU reference (`reference.rs:231-234`).
- **media**: one `AVFormatContext` per decoder with the other streams discarded, so two opens read
  nothing twice (`decode.rs:470-495`); `stts`, `ctts` and `elst` (`mp4.rs:960-1330`); Insta360
  detection (`detect.rs:182-200`); a one-track `Mp4Writer` (`encode.rs:544-569`); VA-API and
  Vulkan decode; zero-copy paths A (DMA-BUF) and B (shared device, the default where the device
  can be shared, D25). media depends on core only today.
- **playback**: a worker per stream (`player.rs:713`); per-stream `pick` (1344-1360) and its
  show-as-it-arrives branch (1092-1101); catch-up per worker (`worker.rs:716-743`); `check_end` on
  stream 0 (1477-1483); `DEFAULT_SLOTS = 4`, `MAX_SLOTS = 6` (`slots.rs:55-56`).
- **core**: `Media` with one `stream` and `deny_unknown_fields` (`project/mod.rs:128-170`);
  `SCHEMA = 1` (`file.rs:40`); `from_json` checks the schema on a `Value`, then parses the text
  again so errors carry a line and column (158-176); 21 `OPS` with an example each
  (`edit.rs:257`); server-side `add_media`
  (`agent/src/ops/edit.rs:271-285`); `Gesture` (`history.rs:222-245`); `TempFile`.
- **export**: one decoder with `HwOutput::Download`, VA-API first (D52; `pipeline.rs:129-160,
  398-420`), one held frame (63-66), one source (1042), `placement` (758-798); D56's own device,
  paused while the viewer plays.
- **app, agent, cli, synth, xtask**: `Pane`/`SIDE` (`app.rs:37-60`), `number()`
  (`inspector.rs:103-163`), `DrawKey` (`viewer.rs:35-40`), the raw view (`app.rs:645-656`); D57's
  and D58's tools; `agent::still`; Insta360 name grouping (`ops/clips.rs:232-260`); synth's scenes
  as functions of direction, `render_view_ideal` (`synth/src/lib.rs:300`); media's
  `encode::mux_video_streams` (`encode.rs:1311`); `SyntheticBackend`,
  `ManualClock`; Tier 2 gates that skip, never pass; one-track perf clips. synth and sphere build
  at opt-level 3 in dev, render and testkit at 2, the rest at 0 (`Cargo.toml:97-114`).
- **Hardware so far.** odin, per lens at the X5's 8K30: Vulkan Video copied 78 fps, path B 81,
  VA-API copied 20 to 43 over
  three runs, unstable (perf.md, slice 5b and the seventh run). The projection pass from 5.7K equirect: 0.20 ms at 1080p,
  0.74 ms at 4K (`hw-reports/odin-0d491ae.md:504-506`). The 4K30 export from 5.7K equirect: 32.5
  fps in H.264 and 44.4 in HEVC, held up by VA-API decode at 22 to 30 ms a frame
  (`hw-reports/odin-6993987-m25.md:145-156`). huginn, VA-API copied, both tracks of a sample:
  2×2880² at 59.94 at 1.61× to 1.95× real time, 2×3840² at 29.97 at 1.10× to 2.40× (perf.md at
  4cd8a0f, unstable; `hw-reports/huginn-tier2-samples.log`). huginn has no Vulkan Video HEVC, and
  no engine or export run on lens pairs.
- **Samples**: the 14 X5 files (7 masters, 7 LRVs, 2.5 GB) are on odin and in the cloud at
  `/home/claude/spherecut-samples/insta360-x5/`, so tests that need only the samples and the CPU
  run where the agents work.

## 3. The X5 file: what is known, what is not

### 3.1 Known [F]

- **Tracks.** Two square HEVC tracks, 3840² at 8K, 2880² at 5.7K, 1920² at 4K120, 8-bit full range,
  BT.709, chroma left, no B-frames, closed GOPs with an IDR every 60 frames at 8K30, 50 at 8K25 and
  120 at 5.7K60 and 4K120, where every other frame is a non-reference TSA_N. Equal frame counts; the rate is constant within 42 ppm.
- **Trailer.** A version 3 `inst` box (so `mp4.rs:348`'s "before X5 firmware 1.7" is wrong). The
  index is record 0 at EOF−388 with 31 slots, each matching its record's 6-byte footer; records sit
  on 128 KiB boundaries. Present: 1 (protobuf metadata), 2 (a 1280×640 NV12 thumbnail, absent from
  the 4K120 file), 3 (IMU, 20 B a sample), 4 (exposure, `{u64 µs, f64 s}` a frame), 9, 10, 11, 22,
  28, 29. Absent: 5, 6, 7, 12. Records 22 and 28 differ in every take (correcting
  insta360.md:55-56).
- **Calibration is per unit.** V1, V2, V3 and V6 are byte-identical in every master, LRV and mode.
  Field 27 = 5376, 5376, 5312, 5312. Field 136 = 4 marks V6 as the one in use. V3 is Mei with
  ξ = 2 plus k1 k2 k3 p1 p2; V6's d0–d4 are radial and agree with V3 within 1.1 px over θ = 60° to
  100°. V6 moves lens 0's cx by +11.9 canvas px and its pitch from +0.211° to −0.271°, and lens 1's
  tx from −4.46 mm to +0.64 mm. tz is −31.5 mm. The model folds at θ = 120.1°; V1's radius is the
  image height at 100°, which matches field 145's 200° field.
- **Halves and order.** The LRV's left half is track 1 and its right half track 0 (correlation
  0.985 to 0.995, against 0.41 to 0.50); record 22 uses the same order; lens 0's block is the
  canvas's left. Upright video stores a roll of about 90°.
- **No per-lens exposure or white balance** anywhere decoded: record 4 is global (no ISO, no gain),
  record 12 is absent, record 28 is a global tone curve.
- **LRV.** One H.264 1664×832 track; each half is its master track scaled by 1.000 (shift
  ≤ 0.5 px); every record but the metadata is byte-identical to the master's. 25 fps for a 25 fps
  master, else 29.97; the master's frame count at 25 and 29.97, half at 59.94, ⌈¼⌉ at 119.88.

### 3.2 What is not known, what it costs, what settles it

| # | Unknown | Cost if wrong [C] | Settled by | Meanwhile, and why |
|---|---|---|---|---|
| U1 | Q2a: which track is the screen-side lens | the default view faces the wrong way; the pane's front and back names swap | take 15's luma rule (8.6) | track 0's axis at yaw 0, lenses named A and B |
| U2 | Q2b: which calibration block goes with which track (the binding) | a relative tilt of up to 2.3° across the seam (44 px at 3840²; at most 1.1°, 20 px, when the roll turns about the lens axis), plus 0.4° to 0.6° along it from the swapped centres (13 to 19 px) [C] | the joint stage-G search (8.4) on clips 8 and 10, then take 14 | `Reversed` (block 0 reads track 1): block 0 is the canvas's left, the LRV's left half is track 1, record 22 agrees [F] |
| U3 | Q4: the focal scale from canvas to video (A or B: side/5312; C: side/5376), and any per-unit scale | 1.2 %: 2.18° across the seam, 41 px at 3840² | take 14's scale fit | C: the format reader's seam test leaned C but does not count; GyroView uses C, orbis A [rep] |
| U4 | Q6: the Euler composition, its quarter-turn roll offset, the axis of the 180° turn | 0.3° to 0.6° per lens (6 to 11 px), across the seam, or a lens upside down; a wrong twin turns the whole picture a quarter or half turn about the forward axis, which the seam cannot see and the up-and-down rule catches (6.2) | stage G and J (8.4): the gross class and the relative roll on clips 8 and 10, the tilt (the fine class) on take 14; among twins, the up-and-down rule (8.6) | the class stage G picks on clips 8 and 10 (M3.6); a placeholder before |
| U5 | the sign of tz | 2.4° across at 1.5 m; nothing at infinity | clip 8, once Q4 is settled (8.6) | `AsStored`: lens 1 sits behind lens 0 on lens 0's axis, the physical layout |
| U6 | Q5: what V6's d5 to d12 mean | 12 to 33 px per lens at 3840², mostly along the seam, with the stored centres held; the readings lie 20 to 36 px apart (8.2) [C] | clips 8 and 10 on the along-seam terms (M3.6), then take 14 | `RadialOnly` (d0–d4) until M3.6, then the reading clips 8 and 10 rank best |
| U7 | V3 or V6 | lens 0: 11 to 17 px from its intrinsics (cx +11.9 canvas px) plus 0.48° of pitch; lens 1: 8 to 13 px plus 0.29° of pitch [C] | take 14, centres held at the stored values | V6: field 136 = 4 [F] |
| U8 | whether the tracks' timing tables are equal (first pts, `elst`, `stts`, `stss`, `ctts`) | the lock design is wrong | P-TRK (M3.0) | none needed |
| U9 | pair decode throughput in the export and on huginn | the 4K30 export or huginn's 5.7K30 target missed | P-DEC (M3.1) | odin's playback numbers |
| U10 | what the lock costs in slots and drops | a torn seam, or drops | P-LOCK (M3.8) | the raw view |
| U11 | the exposure and white-balance mismatch between the lenses | flicker, or a step at the seam | P-GAIN (M3.6, M3.15) | matching off |
| U12 | the stitch shader's cost | the UI misses refresh | P-SHADE (M3.7) | none needed |

Tier 1 proves that each enum value is implemented as documented and differs from the others.
Only Pat's footage says which value is true; no synthetic test is evidence for a convention.

### 3.3 What a scene can settle

The X5's lenses sit back to back on one axis, 31.5 mm apart, with 0.6 mm sideways. The epipolar
curves are great circles through the axis, so parallax moves a seam point **across** the seam
(along θ) only, by b·q_scene: 1.80° at 1 m, 1.20° at 1.5 m, 0.18° at 10 m. A focal-scale error
(U3) also moves points across the seam uniformly, and a relative tilt or a yaw/pitch composition
error (U4) moves them across in a cos φ pattern. On a near scene, both are confounded with depth: a free q per 15° sector absorbs 93 % of a 0.3° tilt
and leaves 0.015°, 0.3 px at 3840² [C]. A relative roll, the difference of the two centres, the
binding's swapped centres (U2) and most of what V6's d5 to d12 do (U6) move points **along** the
seam, which parallax never does. The binding's rotation part is a tilt, across; a lens late under a
turn is a rigid relative rotation, across or along as the turn's axis lies (3.4). So:

- **Clip 8** (a wall at about 1.5 m, something at 3 to 5 m) can rank the binding (through its
  centres), the V6 reading and the gross class (the turn axis, the roll offset and the Euler class
  to within about 2°, up to 6.2's twins), and measure the relative roll and the other along-seam
  terms. It cannot separate focal scale or a tilt from parallax, so the fine Euler class (the tilt)
  waits for take 14. That is why the format reader's seam test on it "does not count".
- **Clip 10** (tripod, the camera turned by hand) puts different scenes and depths behind the
  seams, so a class that wins on clip 8 must also win there, on its frames at rest after a turn (the
  cross-scene condition of 8.4; on moving frames, a lens late by part of a frame is a rigid
  relative rotation).
- **Take 14** (everything beyond 10 m but the ground under the tripod, which 8.4 models as a plane
  at Pat's tripod height) fixes q, so the across-seam terms measure scale and tilt directly; its
  second heading (question 1) is the second scene for the fine class, and its noise floor sets the real-footage gates.
- **The seam cannot see a rotation common to both lenses**, so up and forward come from outside
  checks (8.8), and which lens faces forward is a choice (question 3). For the same reason it
  cannot tell twins apart: Euler conventions, turn axes and roll offsets that predict the same
  relative rotation differ by a common quarter turn about the forward axis (6.2), and the
  up-and-down rule (8.6) picks among them.

### 3.4 The error budget [C]

From Pat's V3 lens 0 (ξ = 2, fx 4284.62 canvas px, k1 0.186, k2 2.050, k3 −3.244) at θ = 90°:
r(90°) = 2408 canvas px, 1720 px at 3840²; dr/dθ = 26.6 canvas px per degree. So **across** the
seam: 19.0 px/° at 3840², 14.3 at 2880², 3.81 at 768², 4.1 at the LRV's 832. **Along** the seam,
a degree of azimuth is r(90°)·π/180: 30.0 px/° at 3840², 6.0 at 768². The attribution test
(8.3) holds this arithmetic to the images.

| Planted difference | Term (8.2) | Size | At 3840² | At 768² (Tier 1) |
|---|---|---|---|---|
| Q4: C against A (5376/5312, 1.2 %) | a0 | 1.09° per lens, 2.18° across | 41 px | 8.3 px |
| V3 against V6, lens 0 (cx +11.9 canvas px, pitch −0.48°, fx −0.17 %) | a1/b1, c1/d1, a0 | 11 to 17 px from the intrinsics, plus 0.48°; a0 0.15° | 11 to 17 px plus 9 px | 2.2 to 3.4 px plus 1.8 px |
| V3 against V6, lens 1 (centre, focal and distortion; pitch −0.29°) | a1/b1, c1/d1 | 8 to 13 px from the intrinsics, plus 0.29° | 8 to 13 px plus 5.5 px | 1.6 to 2.6 px plus 1.1 px |
| V6 reading: {D6D5 with extension, D9 Interleaved}, {D5D6 with extension, D9 ByAxis} or {D5D6, D7 ByAxis} (6.1) against `RadialOnly`, centres held | c1/d1 mostly, a1/b1 | 12 to 33 px per lens; the readings 20 to 36 px apart | 12 to 33 px, 12 to 32 px of it along | 2.4 to 6.6 px |
| Binding reversed, or the tracks swapped (Pat's V6 angles and centres) | a1/b1 (a tilt of twice the lenses' angle difference along the turn axis), c1/d1 (the swapped centres) | tilt up to 2.3°, at most 1.1° with the roll about the lens axis; 0.4° to 0.6° along | up to 44 px across; 13 to 19 px along | up to 9 px across in Tier 1's rig (none where its convention makes the swap a common rotation), plus its centres' difference along |
| Relative roll 0.5° | c0 | 0.5° | 15 px | 3.0 px |
| Relative tilt 0.5° (about an axis in the seam plane) | a1/b1 | 0.5° | 9.5 px | 1.9 px |
| cx + 2 px on lens 1 (at 768²) | a1/b1 and c1/d1 | 0.52° across, 0.33° along | n/a | 2 px |
| Stitch distance ∞ against 1.5 m (b = 31.5 mm) | a0 | 1.20° | 23 px | 4.6 px |
| The same at 10 m (take 14's nearest) | a0 | 0.18° | 3.4 px | n/a |
| tz sign flipped, scene at 1.5 m | a0 | ±1.2°, so 2.4° apart | 46 px | 9.2 px |
| Euler class wrong, with yaw and pitch ≈ 0.3° | a1/b1 phase | about 0.3° to 0.6° | 6 to 11 px | 1.2 to 2.3 px |
| Lens 1 one frame late, turning 10°/s | a1/b1 under a pan or tilt (a turn about an axis in the seam plane), c0 under a turn about the lens axis: a rigid relative rotation ωΔt | 0.33° | 6.3 px across, 10 px along | 1.3 px, 2.0 px |

So Q4, the binding and the V6 reading are very visible on a far scene, and the reading and the
binding's centres are visible along the seam on a near one. The Euler class is visible at the 6 px
level. Its tilt part lies across the seam, where a near scene's depth hides it (3.3). In 77 % of the pairs of kept candidates 0.05° to 2.5° apart, part of the difference is a relative roll along the seam (median 0.45°, 14 px), which clips 8 and 10 can measure [C]. The tilt needs take 14 and a
real-footage floor near 1 px at 3840², which take 14 measures.

## 4. How M3 is verified

### 4.1 The rules

Each slice's reviewer checks these before reading the code.

1. **Every check has a control that must fail**, run on every run. D32's rule ("a control that
   stops failing fails the test", `goldens.rs:30-31`) becomes `testkit::control::must_fail(name,
   result)`, used by every M3 suite, not only the goldens.
2. **Independent instruments.** synth's lens model (pixel → direction) shares no code with
   sphere's (direction → pixel), which the WGSL mirrors. Nor do the extrinsics: synth composes each
   lens's model in slot pixels, `to_rig` and centre from its own `Planted` (its own enums for the
   binding, canvas map, Euler convention and roll offset, turn axis, upright, tz sign and V6
   reading) with its own code, written from 6.2's text, and converts to sphere's types only at a
   test's boundary (7.1). The CPU reference measures the shader.
   The seam estimator samples through the CPU reference, never through the WGSL under test.
   synth prints the calibration strings with its own printer; camera parses them.
3. **Floors before gates.** A Tier 2 number on Pat's footage is reported with the instrument's
   noise floor on the same take. A row is *recorded* until its question is settled, then *gated*
   from the floor with at least a 3σ margin; the switch is a reviewed change.
4. **Failing first.** Every bug fix comes with a test that the slice note shows failing on main.
5. **Cheap Tier 1.** Each new suite has a wall-time budget (section 19.6) and prints its time;
   the slice note records how much `cargo xtask check` grew. Synthetic media live in temporary
   directories, at most 50 MB at once.
6. **The push rule**: a full `cargo xtask check`, compat builds against FFmpeg 8.1.3 and 9.0.2,
   and every new or touched property test at `PROPTEST_CASES=100000`. From M3.6 on, a slice that
   touches `render::seam`, `sphere::rig` or `synth::lens` also runs `cargo xtask fit-matrix` (8.5).
7. **Provenance.** The review lists every outside source the diff used. Nothing may come from the
   Insta360 Media SDK or Studio, telemetry-parser's `rig.rs` (its Insta360 and its DJI one),
   `rig_body_masks.rs`, `rig_mask_coefficients.rs`, `rig_mask_data.rs` or its V2 θ_max table,
   video-types' statements of vendor internals (its `RadtanDistortPro` name and reading of V6,
   Insta360's 98° mask and its 6° blend ending at 93°, DJI's 0.038 blend; video-types 0.1.2
   `src/geometry.rs:1267, 1451-1452, 1622-1630`), rgryta/insta360-stitch, insv-stitch,
   insta360-rs or insta360-autostitcher (understand-research.md §6), or DJI's stitcher constants
   (`seam_overlap_ratio`, the lens-protector curve, mask polygons, the default `far_11` slot;
   dji.md §3.2 and §11.2).
8. **The slice note** lists each new control and each measured number with its floor and machine.

### 4.2 The round-trip ladder

One scene, one rig and one set of thresholds, climbed rung by rung. Each rung is compared with the
rung below and with the analytic ideal (`synth::render_view_ideal`).

| Rung | Path | First in | Compared with | Rule |
|---|---|---|---|---|
| 0 | synth inverse ∘ sphere forward, on points | M3.3 | identity | ≤ 1e-6 px, ≤ 1e-7 rad |
| 1 | synth lens images → CPU reference `render_rig` | M3.4 | the ideal, masked | the thresholds below |
| 2 | the same planes → GPU `StillRenderer` (lavapipe) | M3.7 | rung 1 | D32's reference rule (blessed − 3 dB, never below 36 dB; no channel 0.1 off) |
| 3 | lossless synthetic `.insv` → probe → camera → `Still::open_pair` → CPU reference, then GPU | M3.5, M3.7 | rungs 1 and 2 | identical bytes: the decode is lossless, so the planes hash equal |
| 4 | 11.5's fixture (that file with a tune that isn't the default) → export (libx264, libx265, high; flat on a side seam, and 360) → decode | M3.11 | `render_still` of the test's own `Rig` (11.5) | M2's floors (40 dB luma, SSIM 0.98); the tune dropped must miss |
| 5 | 11.5's fixture → the app's viewer (kittest, lavapipe) → readback | M3.10 | rung 2, with the test's own `Rig` | ≤ 1 LSB; the viewer's `Rig` built with `Tune::default()` must miss |
| 6 | a synthetic packed `.lrv` → the app's viewer with proxies on (the slot ring and `pick_pair`; kittest, lavapipe) → readback | M3.14 | rung 2, downscaled | its blessed floor |
| 7 | rungs 2, 4 and 5 on RADV, VA-API and zero copy | M3.7, M3.10, M3.11, M3.17 (odin) | the same rungs on lavapipe and software | D32's other-driver rule; M2's Tier 2 floors |
| 8 | Pat's footage | M3.6, M3.12 | the seam signature and its floor | section 8.6 |

**The scene** is `Grid` plus a new `SeamLines` (tilted great circles across both seams, a 1°
checker within ±15° of the seam, a luminance ramp for exposure), with `HighFrequency` for the LOD
rows, a frame code on both hemispheres, and a radius that is finite (R) or ∞. **The rig** is
X5-like at 768² per lens over 200°: V3 Mei with ξ = 2 and the format notes' Brown coefficients,
rolls of 90.5° and 89.3°, yaw and pitch of a few tenths of a degree per lens, principal points 2
to 3 px off centre, tz = −32 mm. A second rig uses equidistant lenses with KB distortion
(PROMPT's model, and the no-calibration fallback), which proves the source path is not Mei-only.
**Outputs**: a 512×256 equirect and 90° rectilinear views at 256² on each seam and each pole. The
rung-3 file is the same rig at 2×768², 8 frames, libx265 `lossless=1`.

**Thresholds** (understand-tests.md §2), masked and weighted by cos(latitude): more than 12° from
the seam, PSNR ≥ the blessed value − 1 dB and SSIM ≥ blessed − 0.005 against the ideal, and PSNR at least 3 dB above the
common-mode control's (D32's form: a slip common to both lenses passes every seam check, 3.3, so
only this comparison can see it, even one present at the first bless; a 0.5° turn costs about
6 dB on lines across it in a 1-D model [C]); each lens
alone over its own coverage, the same; in the band, on the 256² seam views where both weights are non-zero, PSNR
against the ideal at most 1 dB below the better of the two lenses alone (DEBUG 4, 5) over the same
pixels (a penalty against the pixels outside the band measures the scene, not the blend: with no
seam at all, a 1° checker at the equirect's 0.70°/px scores about 16 dB below thin grid lines [C];
one against the weighted mean of the two lenses' own errors can never exceed 0 dB, since a weighted
mean's squared error is never above the mean of the squared errors), no channel more than 0.1 off the CPU reference, a misregistration between
the two lenses' strips of at most 0.1 px rms; with lens 1 at +0.5 EV and +5 % red, a step across
the seam under 1 LSB once matched.

**Controls**, each failing on every run: lens 1 yawed 0.5°; lens 1 turned 0.13° about +Y (0.5 px
across the side seams: the band check's control, about 3.6 dB in a 1-D model [C]); f × 1.012 (C
read as A, which proves Q4 measurable); cx + 2 px; equidistant applied to Mei data; the wrong V6
reading; the tracks swapped (no NCC collapse: a tilt of twice the lenses' angle difference along
the turn axis plus their centres' difference along the seam, 3.4); the turn left out; q = 0
against R = 1.5 m; lens 1 a frame late while the rig pans at 10°/s about +Y (a rigid 0.33° turn
about +Y, so it lands on a1); gains off; both lenses turned 0.5° about +Y, the common-mode control,
which passes every seam check (misregistration ≤ 0.1 px, every term under 0.03°) and must fail
the comparison with the ideal. The geometric ones are also attributed to their term (8.3). Feather
0 and mask margin 0 are not rung-1 controls: on matched lenses a hard switch makes no step, and
with h = 5° a lens's weight is 0 beyond 95°, which mip bleed reaches only at λ ≥ 4 [C]. They are
the controls of `dfish_feather` and `dfish_mask_margin` (10.5), on views where they can fail.

### 4.3 Prototypes, with their rules written now

Each prototype is a slice or a unit of one, merges with tests like any slice, and leaves code that
later slices use. Its report records the numbers and which branch fired.

| Prototype | Slice | Measures | Pass | Redirect | Kill |
|---|---|---|---|---|---|
| P-TRK | M3.0, M3.6 | the timing tables of all 14 files; the LRV's start; the lens offset from the pixels | lock by clip time | a per-track offset; an exact seek after differing keyframe landings | redesign the pairing before M3.8 |
| P-DEC | M3.1 | the export's pair decode per path and machine, and its RSS | section 12 as written | Vulkan Video download, then a GPU-resident export decode (M3.11b) | the target goes to Pat |
| P-GEO | M3.6, M3.12 | the fit of section 8 on clips 8 to 10, then takes 14 and 15 | the X5 entry settled | an own calibration (8.7) | M3.12 stops for a design note; real-seam gates wait |
| P-LOCK | M3.8 | unpaired presents and drops, locked against unlocked | the lock as designed | 8 slots for a pair | a design note on the upload schedule before M3.10 |
| P-GAIN | M3.6, M3.15 | the gain between the lenses in the band, per take | matching off by default | Auto by default | none |
| P-SHADE | M3.7 | the projection pass's GPU time on odin | as designed | an analytic Jacobian for LOD, then a cheaper mask | none |

**P-TRK.** `media::mp4::track_timing(file, track) -> TrackTiming {timescale, count, first_pts,
edit, stts, ctts_hash, stss}` (`stts` as its runs and `stss` as sample numbers, a few entries on the
X5, so section 9 can compare them over the common prefix), with a new `stss` reader; the lock
reads it through `MediaBackend::track_timing` (9); the Tier 2c test
`samples_x5_tracks_equal` runs it on the 7 masters and 7 LRVs. *Pass:* both tracks of every
master agree on every field, and section 9 pairs by clip time. *R1, a constant offset* in the
first pts or `elst`: the offset is passed as `LockSpec.offset` (9), checked at open, removed before the lock compares
times, and kept in the X5's `Known` entry. *R2, the `stss` tables differ:* a pair's keyframe seek
is followed by an exact seek to the later landing, and the seek gate is re-measured. *Kill, the
`stts` tables differ over the common prefix* (frames that don't line up by time): M3.8 waits for
a design note on nearest-pts pairing. Counts that differ only at the end don't kill: the pair
ends at the shorter lens (9). *LRV:* passes if
its clip time 0 is within one master frame of the master's; otherwise the offset goes into the
proxy's time map (15). *Pixels (M3.6):* `samples_lens_offset_is_zero`: on clips 9 and 10, the
seam rms with lens 1 shifted by k ∈ [−2, 2] frames is lowest at k = 0 by 5σ, where the margin is the mean, over disjoint 1 s windows of the same clip in which lens 0 turns by
more than 0.1° a frame (measured as in 8.4), of each window's seam rms at the shifted k less its rms
at k = 0, and σ is that mean's standard error: a window's own difference cancels what lies behind
the seam, and at rest a whole-frame shift changes nothing; otherwise the lock gains an offset,
as R1. A whole-frame sweep cannot see a sub-frame offset, so the test also regresses the per-frame
a1, b1 and c0 on lens 0's own turn between frames (8.4): the slope is the offset in frames,
recorded with its standard error.

**P-DEC.** Two-track perf clips (2×3840² at 29.97, 2×2880² at 29.97 and 59.94; MP4, coded like
the X5, 7.2) and `bench_export_pair_decode`: two `HwOutput::Download` decoders as the
export runs them, serial and one thread per lens, on VA-API, Vulkan Video and software, over the
8K30, 8K25, 5.7K60 and 4K120 samples and the synthetic clips, with the RSS high-water mark.
Playback's pair decode is already measured on odin (section 2).
- *K1, odin's export:* pass if the best download setup decodes the 8K30 pair at ≥ 36 fps a lens
  (30 × 1.2). *K1's rule,* which K2 and K4 share: the gated rows (8K30 from clip 9, the longest,
  for K1 and K4; the synthetic 2×2880² at 29.97 for K2) decode 600 frames a lens, timed from both
  lenses' first frame (opening is reported apart), beside a VA-API H.264 encode of a fixed
  3840×2160 frame at 30 fps through `EncoderSetup::vaapi` (`crates/export/src/encoder.rs:137`),
  since the export's encoder shares the video engine (D56). The slower lens's median over three
  runs is the verdict, and a run under 30 fps fails it. The other modes are recorded once, alone.
  The setup is already measured without the encode: two download decoders on a thread each is
  `bench_samples_realtime_decode`'s `decode_rate` (`crates/media/tests/tier2_samples.rs:366-400`).
  On odin its VA-API 8K30 pair has come out at 0.73 to 1.04× real time in every run but the slow
  allocator's 0.43×, and Vulkan Video's at 1.92 to 2.05× [F] (`docs/perf.md:298, 352, 398, 470,
  544, 612-614, 680-681`): 22 to 31 fps a lens against 58 to 61 [C]. So VA-API download is
  expected to fail K1 and Redirect 1 to fire, and M3.1 drafts D52's update either way.
  *Redirect 1:* pairs decode with Vulkan Video download wherever doctor says it decodes HEVC (an
  update to D52), and VA-API download stays the path where it doesn't (K4). *Redirect 2:* M3.11b
  decodes onto the export's own device through path B (Vulkan) or A (VA-API), never through RAM.
- *K2, huginn's 5.7K30:* pass if VA-API copied decodes the 2×2880² pair at 29.97 at ≥ 36 fps a
  lens, by K1's rule in the export's pair decode and unthrottled in the engine; the samples log's
  1.95× at 59.94 suggests a wide margin [G]. The engine half needs a new row:
  `bench_playback_x5_raw` plays samples only (`require_x5_modes`,
  `crates/playback/tests/tier2_playback.rs:1380-1395`), so M3.1 gives it the two-track perf clips,
  both tracks opened as a sample's are. Path A (`SPHERECUT_ZERO_COPY=1`) is measured beside the
  copy in the same paste. If its numbers beat the copy's and `zero_copy_matches_copy_path` passes
  on huginn (path A had one unexplained mismatch in about a dozen runs on odin,
  `PLAN.md:161-162`), D25's revisit rule applies as Pat wrote it: path A becomes the default where
  Vulkan Video can't decode. The note of the slice that records the answer tells Pat. If path A is the faster but the match fails there, it stays opt-in and Pat gets a decision card with both results, since D25's rule names only the numbers. If the export
  half fails, huginn's export takes K4's redirect.
- *K3, memory:* `bench_export_pair_decode` holds frames as section 12's export will: per lens the
  decoder's current and next frame, `DECODED_AHEAD` = 2 in a bounded channel and one at a consumer,
  so 5 a lens and 10 a pair, 221 MB at 8K [C]. Each run's peak RSS growth is measured as the test
  helper `peak_rss_growth_kib` does (VmHWM reset through `/proc/self/clear_refs`,
  `crates/media/src/mp4.rs:1897-1910`, moved to testkit), in a child process of its own as
  `crates/export/tests/files.rs:137` spawns the test binary, so one run's freed heap can't hide the
  next one's growth. *Baseline:* the same run with lens 0 alone. *Pass:* at 8K30 on each path, the
  pair grows at most twice the one-lens run, plus 10 % [G]; the growth is recorded beside the 221
  MB the frames account for, so the decoders' own share shows. *Redirect,* in this order: the
  software fallback's frame threads per decoder (each holds a 22 MB frame at 8K); the download
  pool's size; `DECODED_AHEAD` 2 → 1 for pairs (44 MB at 8K [C]); then M3.11b, which holds no
  frame in RAM.
- *K4, huginn's 8K30 export:* pass if two VA-API download decoders, a thread each, decode the 8K30
  pair at ≥ 36 fps a lens by K1's rule. huginn had no Vulkan Video HEVC at its last run
  (`docs/perf.md:241-244`; if doctor finds it now, Redirect 1 applies there as on odin), and its
  VA-API 8K30 pair has measured 1.10× to 2.40× real time, 33 to 72 fps a lens [C], unstable, before
  d8a9054 [F] (`docs/perf.md:252`, `hw-reports/huginn-tier2-samples.log:12`). *Redirect:* if K4 or
  K2's export half fails, M3.11b gains huginn's row: the export decodes through path A onto its own
  device there, measured in huginn's next paste. If that misses too, the target goes to Pat as a
  decision card (section 12).
- *huginn's prompt* (M3.1 (c)) is one paste. It runs the full hw-test that `hw-run-6.md` already
  asks of huginn (H1, H2 and M2's 4K30 row, all still pending, `PROGRESS.md:2252-2263`), which now
  holds `bench_export_pair_decode`, `bench_playback_x5_raw` with the perf clips (on VA-API copied,
  then again with `SPHERECUT_ZERO_COPY=1` for path A) and `zero_copy_matches_copy_path`. It says
  first how long the perf clips take to make: odin made M2's three in 3838 to 3875 s [F]
  (`docs/perf.md:457, 533`), and the three two-track clips have as many pixels again [C], so expect
  about two hours of clips at odin's pace before any bench [G]. M3.1 merges on odin's K1 and K3;
  K2 and K4 go into perf.md whenever huginn answers, and M3.17 takes them up if it hasn't.

**P-LOCK** is the lock group itself (section 9), with `unpaired_presents`, `pair_wait` and
`pair_resyncs`. *Pass:* in Tier 1, 0 unpaired by tag and by content in the mixed test, and the
pacing rule met in every case of `a_locked_pair_paces_to_its_slowest_lens`; on odin,
`bench_playback_x5_raw` locked at 8K30, 5.7K60 and 4K120 makes 0 unpaired, presents ≥ 99 % of
frames, and drops at most 1 percentage point more than the unlocked raw view. At 8K25, where
paths A and B decode-drop 19 to 29 frames in real time unlocked (perf.md), the locked run on path
B makes 0 unpaired, meets the pacing rule, and records the pairs presented and `pair_wait` during
the drops. huginn's VA-API rows join when its prompt
comes back. *Redirect:* `MAX_SLOTS` 6 → 8 for a locked pair (236 MB of
VRAM at 8K). *Kill:* M3.10 waits for a design note on the upload schedule.

**P-GAIN.** The fit's report gives, per take and channel, the median log ratio between the lenses
in the band and how much it moves. Matching is off by default if the ratio stays under 1 % and
moves under 0.5 % on every take, take 17 included; Auto otherwise, and M3.15's note gives Pat
each take's numbers and the default that follows. **P-SHADE.** The
projection pass's GPU time on odin's RADV (overlay timestamps), stitched against equirect.
*Pass:* ≤ 0.8 ms at 1080p and ≤ 2.0 ms at 4K. *Redirect:* an analytic Jacobian for the LOD, which
saves 8 of the 10 lens projections, then a cheaper mask.

### 4.4 Tiers

- **Tier 1**: the cloud box (lavapipe, software codecs, 4 cores, a tight disk), no samples.
- **Tier 2c**: tests that need Pat's samples and the CPU only, marked `#[ignore = "tier2: …"]`, so
  that agents can run them in the cloud while they implement; on odin hw-test runs them with the
  rest. They gate on a new `testkit::tier2::require_samples_cpu()` (M3.0), which needs only
  `SPHERECUT_SAMPLES` and skips when the folder is missing; `testkit::samples::role` resolves
  through it. The old gates can't serve: `require_samples_dir()` and `require_sample()` call
  `require_hw_run()` first, which skips unless hw-test has set `SPHERECUT_TIER2`
  (`testkit/src/lib.rs:69-80, 108-109`) [F], so they stay for Tier 2h. The cloud command, with
  `SPHERECUT_TIER2` unset, is
  `SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 cargo test -p <crate> --test <file> -- --ignored <name>`.
- **Tier 2h**: RADV, VA-API, zero copy, real time and the GUI, on odin by Remote Control and on
  huginn by a prompt Pat pastes. Nothing blocks on huginn: a slice merges on odin's numbers,
  huginn's rows read "pending huginn" until its answer comes, and they go into perf.md, with any
  rule they fire (K2, K4), whenever it does.
- **The role map.** `testkit::samples::role("tripod_seam")` and the like read a checked-in
  `samples-roles.toml` (file name to role: `tripod_seam = 008` for clip 8, `open_field` for take 14 once its file arrives (the camera numbers its own files, so clip 1 is 004), …), since
  nothing parses NOTES.txt.

## 5. The camera crate (`spherecut-camera`, new)

### 5.1 Reader

`crates/camera`, `#![forbid(unsafe_code)]`, depends on core (paths, `TempFile`), sphere (the types
it converts into, from M3.5) and thiserror; no FFmpeg. Its inputs are `impl Read + Seek`, so
agents' `openat2` roots (D45) and the app's jobs open the file themselves; D24 confines FFmpeg's
opens, not ours. **No read on the UI thread.** The app's clippy.toml bans blocking calls in app
code only (`crates/app/clippy.toml:59-62`) and leaves other crates' code to the runtime guard (its
header) [F]. So every camera entry point that reads (`read_trailer`, `read_record`,
`verify_companion`, and `profile`'s list, load and save, 17) calls `core::blocking::may_block`
first, as media's reads do (`blocking.rs:84`); `companions` reads nothing. The app calls the
readers only from the player's `OpenJob` (`worker.rs:105`, which the proxy toggle's reopen goes
through), the making job (`reframe.rs:576`) and the profile jobs (13.5). A camera test runs each
reader on a thread marked as the UI thread and expects a panic. media depends on camera (probe and `media::project`); camera never depends on
media, so there is no cycle. `Cargo.toml` gains a `spherecut-camera` row at opt-level 2, so its
100k-case properties stay inside their budget.

```rust
pub mod insta360 {
    pub fn read_trailer<R: Read + Seek>(r: &mut R, len: u64) -> Result<Option<Trailer>, CameraError>;
    pub struct Trailer { pub version: u32, pub payload_start: u64, pub records: Vec<RecordRef> } // id, format, offset, size
    pub fn read_record<R: Read + Seek>(r: &mut R, t: &Trailer, id: u8, max: usize) -> Result<Option<Vec<u8>>, CameraError>;
    pub fn parse_metadata(bytes: &[u8]) -> Result<Metadata, CameraError>;      // camera::pb::Walker, hand-written
    pub fn parse_exposure(bytes: &[u8]) -> Result<Vec<Exposure>, CameraError>; // record 4: {t_us, seconds}
    pub fn thumbnail(bytes: &[u8]) -> Result<Nv12Image, CameraError>;          // record 2: 40-byte header, 1280×640 NV12
    pub mod calib { pub fn parse(gen: Generation, s: &str) -> Result<Calib, CameraError>; pub fn print(c: &Calib) -> String; }
    pub fn companions(path: &Path) -> Companions;     // names only, no I/O: master to LRV and back; moved from agent/src/ops/clips.rs:232-260
    pub fn verify_companion<M: Read + Seek, L: Read + Seek>(master: (&mut M, &Trailer), lrv: (&mut L, &Trailer))
        -> Result<Pairing, CameraError>;            // reads record 4 of both
}
```

`Metadata` holds the serial, model, firmware, first-frame time, frame period, file group, window,
LRV size, fields 79, 80, 131, 136 and 145, the calibration strings and a count of unknown fields.
Record 3 (the IMU) is read for its length and stamps only; M4 parses it. **Fields read**
(insta360.md §3.5 and the format report): 1, 2, 3, 24 to 27, 44, 79, 80, 93, 98, 131 to 136 and
145, and the strings V1 5/17, V2 53/55, V3 54/56 and V6 111/112. **Limits** (D15's style): the
trailer ≤ min(file, 1 GiB), the metadata ≤ 1 MiB, a string ≤ 4 KiB, ≤ 64 index slots; a record past
the trailer is skipped and counted, never followed; no allocation larger than a checked size; each
error (`Truncated`, `SlotPastEnd`, `FooterMismatch`, `BadVarint`, `TokenCount {generation,
found}`) carries its offset, and no input panics. Records 9, 11, 22, 28 and 29 are listed with
their sizes only.

### 5.2 Calibration strings

`Calib { generation, lenses: [LensTokens; 2], canvas: (u32, u32), word: u32, lens_type: u32 }`,
where `LensTokens` holds named values (xi, fx, fy, cx, cy, ypr, t, coefficients in token order)
in canvas pixels. The grammar is insta360.md §4.1: token counts 6N + 4, 16N + 2, 19N + 2 and 27N +
2 (16, 34, 40 and 56 for two lenses), the leading lens count, the packed word whose top bits give
the generation. Values keep their token strings, so `parse(print(c)) == c` bit for bit and
`print(parse(s)) == s` for every string in Pat's files. V6's 13 terms stay in token order;
geometry reads them (6.1). `camera::insta360::to_calibration(&Calib) -> sphere::rig::Calibration`
makes block 1's centre local to its square (cx − 5376) and is added in M3.5, the first slice that
needs both crates; M3.2 does not wait for sphere's types.

### 5.3 The built-in geometry table

`camera::insta360::known::lookup(model, firmware) -> Known` reads a static table. A `Known` holds
the model ("X5"), `measured_on` (the firmware its evidence covers, `["v1.1.22_build1"]`), a
`sphere::rig::Geometry` with its status and evidence, `lrv_half_track: [1, 0]` (the left half is
track 1 [F]) and `screen_side_track: Option<u8>` (take 15, M3.12). A file whose firmware
`measured_on` doesn't list gets the entry as provisional, with evidence that says so. A model with
no entry gets the X5's conventions, provisional, with the notice "geometry assumed from the X5".
`known` lands in M3.5 beside `to_calibration`, with probe's `lenses.binding` and `geometry`
blocks. Before M3.6 the X5's extrinsics are a placeholder (the identity `EulerConv` with the −90°
roll offset), with the evidence "placeholder until the fit". M3.6 replaces it with what its run on clips 8
to 10 finds, whatever the rules decide (8.6), and keeps it only if those clips fail 8.6's gross
rule; `Known.stitchable` is then false, and the app shows such files as lenses (13.2). Firmware
updates and later bodies need a table row, not code.

### 5.4 Companions, probe, doctor and corrections

- **Companions.** `companions` pairs `VID_…_00_NNN.insv` with `LRV_…_01_NNN.lrv`;
  `verify_companion` says `Verified` when record 4 is byte-identical in both files (true on all 7
  pairs [F]) and `NameOnly`, with a notice, otherwise. agent's clips.rs calls it, and its tests
  pass unchanged.
- **Probe** (`media::probe`, cli, and agent `ops/probe.rs:244-295`) keeps `camera` in its pinned
  shape, since within a version fields are only added (`docs/cli-json.md:49-53`) [F]. The CLI's
  `camera` object (`CameraGuess`, `media/src/detect.rs:40-46`; `cli-json.md:366`) gains `serial`
  beside `vendor`, `model`, `firmware`, `layout` and `evidence`. The agent's summary keeps
  `camera` a name string or null (`ops/probe.rs:292, 365-376`) and gains `camera_detail {vendor,
  model, firmware, serial}`. Both
  gain `lenses {tracks, binding, evidence, screen_side}`, `calibration
  {generations, in_use, canvas, window, hash}` (the snapshot's
  hash, 11.1), `geometry {status, evidence}`, `companion {path,
  verified}`, `exposure_entries` and `records [{id, size}]`, documented in `docs/cli-json.md` and
  pinned by a contract test.
- **Doctor `--file`** says whether the file can be stitched (calibration present), whether both
  lenses decode on the chosen path, and, if not, what is missing: "Insta360 X5 v1.1.22, geometry
  provisional".
- **Corrections in M3.0:** `mp4.rs:348`'s comment; insta360.md (records 22 and 28 differ in every
  take; the LRV is 25 fps for a 25 fps master, not "always 29.97"; the LRV's half order is fact;
  §4.3's `RadtanPro` bullet is marked Studio-derived, as §8 marks telemetry-parser's rig, 4.1
  rule 7).

### 5.5 Tests

**Tier 1 properties** at 100k: the trailer round trip (through synth's writer, 7.2); a trailer cut
at every point is an error, not a panic; arbitrary bytes never panic; varints round-trip; unknown
fields are skipped; strings round-trip; a wrong token count is an error. **Cross-implementation:**
`camera::insta360::calib::parse(synth::insv::print(truth))` gives the truth's numbers back
(synth's printer and camera's parser share nothing). **Bad files:** every trailer `InsvFault` (7.2) is a
clear error naming the record. **Tier 2c:** `samples_insv_trailer_parses` on all 14 files (index,
records 1, 3 and 4, four generations, strings equal across masters and LRVs, field 27, frame
counts as in the format report) and `samples_calib_print_is_identity`. There is no
telemetry-parser oracle: the records' own footers, the index and the byte-identical reprint check
the walk on every file (D62).

## 6. Lens maths (`sphere::fisheye::Unified`, `sphere::rig`)

### 6.1 The unified model and one canonical distortion

```rust
pub struct Unified { pub xi: f64, pub fx: f64, pub fy: f64, pub cx: f64, pub cy: f64, pub dist: Canonical }
pub struct Canonical {      // every reading of V3 and V6 maps onto this form
    pub k: [f64; 5],        // radial r², r⁴, r⁶, r⁸, r¹⁰
    pub p: [f64; 2],        // tangential p1, p2 (Brown)
    pub pe: [f64; 2],       // their r² extensions p1', p2'
    pub s: [f64; 4],        // thin prism: x gets s1 r² + s2 r⁴, y gets s3 r² + s4 r⁴
}
impl Unified {
    pub fn project(&self, p: DVec3) -> Option<DVec2>;   // lens frame → pixel, closed form
    pub fn unproject(&self, px: DVec2) -> Option<DVec3>; // CPU only
    pub fn limit_theta(&self) -> f64;                    // the largest θ at which r(θ) still rises
}
```

```
m = (x, y)/(z + ξ),  r² = |m|²,  R = 1 + k1 r² + k2 r⁴ + k3 r⁶ + k4 r⁸ + k5 r¹⁰
P1 = p1 + p1' r²,  P2 = p2 + p2' r²
xd = mx R + 2 P1 mx my + P2 (r² + 2mx²) + s1 r² + s2 r⁴
yd = my R + 2 P2 mx my + P1 (r² + 2my²) + s3 r² + s4 r⁴
u = fx xd + cx,  v = fy yd + cy
```

| Source | k | p1, p2 | p1', p2' | s1..s4 |
|---|---|---|---|---|
| V3 | k1 k2 k3 0 0 | p1, p2 | 0, 0 | 0 |
| V6 `RadialOnly` | d0..d4 | 0, 0 | 0, 0 | 0 |
| V6, any `V6Reading` (below; 0 where a field is none) | d0..d4 | `tangential`'s pair, in its order | with `extension`, d7 and d8, in the pair's order | `prism`'s four, in its order |
| for example, GyroView's and raw360-formats' k1..k5, p1..p4, s1..s4 | d0..d4 | d5, d6 | d7, d8 [G: p3, p4 read as r² extensions] | d9..d12 |
| for example, orbis's k1..k4, reserved, p1, p2, s1..s4, reserved ×2 | d0..d4 (orbis reserves d4; 0 on Pat's unit [F]) | d5, d6 | 0, 0 | d7..d10 |

**V6's d5 to d12** are read through a small generic space, as 6.2 reads the Euler angles, so no
source's reading is copied or favoured (video-types names one after a vendor-internal class, and
4.1 rule 7 keeps its statements out). d0..d4 are always radial: orbis reserves d4, which is 0 on
Pat's unit [F], so nothing turns on it.

```rust
pub struct V6Reading { pub tangential: Option<Tangential>, pub prism: Option<Prism> } // RadialOnly: both None
pub struct Tangential { pub pair: PairOrder,  // D5D6 (p1 = d5, p2 = d6) | D6D5 (p1 = d6, p2 = d5)
                        pub extension: bool } // p1', p2' from d7 and d8, in the pair's order
pub struct Prism { pub at: PrismAt,           // D7: d7..d10, only without an extension | D9: d9..d12
                   pub order: PrismOrder }    // ByAxis: x 1st, 2nd; y 3rd, 4th | Interleaved: x 1st, 3rd; y 2nd, 4th
```

That is 21 values. GyroView's and raw360-formats' reading is {D5D6 with extension, D9 ByAxis},
orbis's {D5D6, D7 ByAxis}; the space holds every pair order, extension and prism layout, and no
value is named after a source. It is fixed in M3.3's first commit, before M3.9 writes it into
schema 2. Each value has a property test: its own formula, written term by term from its fields,
equals the canonical evaluation, and the two sourced readings, written out as their sources state
them (orbis's where d4 = 0), equal the values named above,
so a slip in a permutation is caught. The shader evaluates only
the canonical form (10.3), so every reading the Stitch pane offers renders on the GPU exactly as
the CPU and the fit see it.

- **Frames.** The lens frame is Mei's and OpenCV's: +x right in the image, +y down, +z along the
  optical axis. sphere's KB lenses look down −Z with +y up (`fisheye.rs:1-11`); F = diag(1, −1, −1)
  maps one to the other, with det +1.
- **Placement.** `Unified` sits beside the KB models, not as a fifth `Model`, because Mei distorts
  the plane point, not θ (`fisheye.rs:9-11`). `LensModel` is `Mei(Unified)` or `Kb(KbLens)`, where
  `KbLens` is an existing base model with k1..k5 (DJI needs k5, dji.md §3.3), a focal, a centre
  and the same tangential terms. Its base applies after the polynomial, r = base(θ_d), as
  `Lens::radius` does (`fisheye.rs:52-60`), and the shader carries the base per lens (10.3), so
  every base renders on the GPU as on the CPU and in the fit.
- **The inverse** repeats one pass until the full forward's residual is under 1e-12 in normalised
  plane units, at most 20 passes: strip the tangential and prism terms at the current estimate,
  then a Newton on m·R(m²) = r_d bracketed on [0, m(`limit_theta`)]. Then comes the closed-form
  lift η = (ξ + √(1 + (1 − ξ²)ρ))/(ρ + 1). Pat's V3 and V6 numbers need 6 or 7 passes anywhere up
  to 119°, and 6.3's scaled draws at most 10 [C]. Three passes, Gyroflow's count, leave 1.3e-7 to
  2.1e-6 rad at θ ≤ 100° on Pat's numbers, which 6.3's tolerances refuse [C]. The method follows
  Gyroflow's `insta360.rs` (GPL-3.0-or-later, attributed), whose comment says its old fixed-point
  loop "silently gave up … from ~74° on the X5". It returns `None` past the limit, when the
  bracket fails, or when 20 passes leave the residual above 1e-12.
- **The limit.** For ξ > 1 the projection folds at acos(−1/ξ) (120.0° at ξ = 2); for ξ ≤ 1 the
  denominator z + ξ vanishes at acos(−ξ) (90° at ξ = 0). The polynomial can fold earlier, so
  `limit_theta` scans dr/dθ in 0.05° steps up to that bound. `Rig::build` refuses any θ_max
  beyond it. On Pat's V6 the curve stops rising at 120.1° [F]; θ_max is 100°.
- **The frame.** The X5's square frame cuts its image circle on all four sides; the circle runs to
  104° to 107°, by the canvas map (r = 1976 to 1983 px at 3840², understand-format.md §3) [C]. So
  `rig::theta_frame` finds each lens's θ_frame, the smallest θ at which its circle meets its
  slot's edge: θ along a straight edge has one minimum, near the foot of the perpendicular from the
  centre, which a golden-section search through the inverse finds. Every canvas map scales with the
  side, so it needs no slot size. `Rig::build` keeps it per lens. On Pat's V6 numbers it is 100.5°
  (lens 0) and 100.7° (lens 1) under map C, and 99.1° and 99.2° under A [C]. θ_max may exceed it,
  since 10.3's frame term masks every sample whose footprint leaves its slot; the feather band may
  not (11.2), so the weights stay continuous.

### 6.2 The rig, its frames and its geometry

The **rig frame** is Spherecut's world at zero stabilisation (D33). The front lens looks along
−Z. Which track is the front one is the media's `front_track` (11.1), from question 3; when it is
block 1's track, `build` adds a 180° turn about +Y to both lenses, a common rotation the seam
cannot see. For calibration block b:

```
lens_from_rig_b = E_b · F · Q_tune_bᵀ · T_b · S
  S         U · Y: Y is identity, or the 180° turn about +Y that puts the front track's lens at −Z;
            U is the quarter turn about −Z that geometry.upright names (0, ±90° or 180°; the twins below)
  T_b       rig → block b's view frame: identity for block 0; for block 1, a 180° turn about
            geometry.turn (the strings omit it, insta360.md §4.5)
  Q_tune_b  D33's Ypr of the tune in that view frame (tune yaw > 0 turns the lens right, as seen)
  F         diag(1, −1, −1), fixed
  E_b       block b's stored yaw, pitch and roll, composed by geometry.extrinsics
c_0 = 0,  c_1 = rig_from_lens_0 · (sign(tz) · t_1),  o = (c_0 + c_1)/2
p_b = normalize(d − q (c_b − o)),  d_lens_b = lens_from_rig_b · p_b
```

```rust
pub struct Geometry {
    pub binding: Binding,        // Direct: block b reads track b; Reversed: track 1 − b
    pub canvas: CanvasMap,       // A | B | C | Fitted { scale }
    pub extrinsics: Extrinsics,  // Stored(EulerConv) | Fitted { relative: DQuat, translation_m: DVec3 }
    pub turn: TurnAxis,          // Y | X
    pub tz: TzSign,              // AsStored | Negated
    pub upright: QuarterTurn,    // 0, +90°, −90° or 180° about −Z (U above; set by 8.6's up-and-down rule)
    pub generation: Generation,  // V3 | V6
    pub v6: V6Reading,           // what d5..d12 mean, a generic space of 21 values (6.1)
    pub status: Status,          // Provisional | Settled | Fitted | User
    pub evidence: String,        // a sentence and a docs path
}
pub struct EulerConv {
    pub axes: [Axis; 3],          // the axis each stored angle (yaw, pitch, roll) turns about, a permutation
    pub order: [Angle; 3],        // composition order, first applied first
    pub signs: [Sign; 3],
    pub roll_offset: QuarterTurn, // 0, +90°, −90° or 180°, added to the stored roll
}
```

**The Euler space** is 6 axis assignments × 6 orders × 8 sign patterns × 4 roll offsets = 1152
values. It is generic, so no reader's composition is copied or favoured (telemetry-parser's comes
from Studio, insta360.md:784-795). It has **no direction field** (rig to lens, or lens to rig): the
inverse of R_a(α)R_b(β)R_c(γ) is R_c(−γ)R_b(−β)R_a(−α), the
reversed order with every sign flipped, which the space already holds. With a direction field
every value would appear twice and the distinctness property (6.3) would fail. The quarter-turn
offset matters because upright X5 video stores a roll of about 90° [F] and the readings in
insta360.md §4.5 include "roll less 90°" and "pitch + 90°". **Twins.** The seam sees only the relative rotation, so values of (turn axis, roll offset,
`EulerConv`) that predict the same one are twins: their lens frames differ by one rotation common
to both lenses [C]. When the roll is applied first, a 180° offset is exactly a common 180° turn
about the roll's own axis, and with the roll about z (the lens's optical axis) the ±90° offsets
pair turn axis Y with X, up to a common quarter turn about Z. When the roll is applied second or
last, a 180° offset changes the relative rotation by 0.2° to 1.24° with Pat's V6 angles, and,
with the roll about z, (X, ±90°) lies 0.46° to 0.59° from (Y, 0) [C]. Stage G (8.4) keeps only values that put block 0's
lens within 5° of −Z: each one dropped is either an exact twin of a kept one, turned by a common
rotation that puts lens 0 behind or sideways (640 for each binding), or does not put the lenses back to back (384), but for 128 for each binding: with the roll applied second, about x with turn axis X or about y with turn axis Y, and an offset of 0° or 180°, they put the lenses back to back along X or Y and predict a relative rotation 0.27° from every kept one [C]. A camera that composes that way lands as `Extrinsics::Fitted` (8.4), not on a class. Among the kept values, twins differ only by a common
quarter turn about Z, which `upright` undoes: a quarter turn about −Z (0, +90°, −90° or 180°) that
the up-and-down rule (8.6) sets for whichever member a class reports. The Tier 1 property
`euler_twins_are_quarter_turns` lists the twin groups for sampled stored angles and checks that
their common rotations are exactly these.

**Canvas maps.** For a square track of side s, with src = 5376 per lens, dst = 5312 (field 27) and
c_local = c minus the block's offset (block 1's cx is 8075.73 − 5376):

| Map | Focal | Centre |
|---|---|---|
| A | f · s/dst | c_local · s/src |
| B | f · s/dst | (c_local − (src − dst)/2) · s/dst |
| C | f · s/src | c_local · s/src |
| `Fitted {scale}` | C's, times the scale | C's |

An LRV half uses s = 832, which is exact (each half is its track scaled by 1.000 [F]).

```rust
pub struct Tune { pub lenses: [LensTune; 2], pub feather_deg: f64, pub q_per_m: f64 } // no gains (11.1)
pub struct LensTune { pub ypr_deg: [f64; 3], pub centre_px: [f64; 2] /* canvas px */, pub radius_scale: f64, pub xi_delta: f64 /* 0 unless 8.7's own calibration fits it */, pub theta_max_deg: f64 }
pub struct Rig { pub lenses: [RigLens; 2], pub q: f64, pub feather: f64, pub margin_texels: f64 }
pub struct RigLens { pub track: usize, pub model: LensModel /* in its slot's pixels */, pub lens_from_rig: DQuat,
                     pub centre_m: DVec3, pub theta_max: f64 }
impl Rig {
    pub fn build(cal: &Calibration, geo: &Geometry, tune: &Tune, front_track: u8, sides: [u32; 2]) -> Result<Rig, RigError>;
    pub fn ray(&self, b: usize, d: DVec3) -> DVec3;               // parallax-corrected, in lens b's frame
    pub fn texel(&self, b: usize, d: DVec3) -> Option<DVec2>;
    pub fn weights(&self, d: DVec3, lod: [f64; 2]) -> [f64; 2];
    pub fn unproject(&self, b: usize, px: DVec2) -> Option<DVec3>;
}
```

The tune's centre is in canvas pixels, so one tune serves the master and the LRV; it multiplies the
focal by `radius_scale` and sets θ_max (the pane's FOV is 2·θ_max).
**No gains here.** Under Auto the gains change at every frame (16), and sphere cannot name a core
type (core depends on sphere, `core/Cargo.toml:15`) [F], so the gains and their Auto track are
the media's, in core (11.1). `Rig::build` runs once per source revision; the frame's two linear
RGB gains, from core's `Gains::at`, go to `render::rig::pack` and `reference::render_rig` (10.4)
with the rig. **Stitch distance** is stored
as q = 1/D in m⁻¹ (q = 0 is ∞) and shown in metres: the ray correction is exact
(understand-research.md §3) and the disparity b·q is linear in q. **Weights:** w_b = 1 −
smoothstep(90° − h, 90° + h, θ_b) on the parallax-corrected ray, zero where θ_b > θ_max −
margin(λ_b) or where the sample's footprint leaves its slot (10.3's frame term),
then normalised. margin(λ) is 2^(λ+1) texels in degrees at θ_max, about how far area
mips pull the black corners into the circle; h shrinks (never below 0.5°) when the margin or the frame
(θ_frame, 6.1) leaves
less overlap than h. The default h is 5°: a 10° band, shown as "seam width 10°".

### 6.3 Property tests (100,000 cases)

- **Unified:** `unproject(project(d))` within 1e-7 rad of d, and `project(unproject(px))` within
  1e-6 px of px, in the lens's own pixels (a slot's, or the canvas's), for ξ ∈ [0, 2.5] and θ ≤
  min(100°, `limit_theta` − 1°), where `limit_theta` includes the polynomial's fold. The generator
  draws ξ, then coefficients around Pat's V6 values (the radial ones × 0.8 to 1.2, the rest × −1.5
  to 1.5) scaled to that ξ: a term of degree n in m is multiplied by (m_X5/m_top)^(n − 1), where
  m_top is the plane radius at min(100°, the model's end − 1°) for that ξ and m_X5 the X5's at
  100°. The polynomial then keeps the X5's shape over the drawn range at every ξ, and that range
  reaches at least 88° [C]. Unscaled, Pat's V3 k1 to k3 fold at 38° for ξ = 0 and before 100° for
  every ξ up to about 1.43 [C], and discarding those draws would leave small ξ untested. ξ = 0 gives
  r = tan θ and ξ = 1 gives tan(θ/2) (no distortion); r(θ) rises up to `limit_theta`; `unproject`
  is `None` past it.
- **Canonical:** V3's and each of the 21 `V6Reading` values' own formulas equal the canonical form
  (6.1).
- **Rig** (the small enums exhaustively, `EulerConv` sampled): at q = 0, `unproject(texel(d)) ==
  d`, the turn included; det(`lens_from_rig`) = +1; two different values of any field differ in
  texel for some ray at stored angles of at least 1°, so no two values are silently the same code;
  canvas maps A, B and C invert; flipping the binding swaps the tracks and nothing else;
  distinctness covers all 21 `V6Reading` values, on a calibration whose d5..d12 are all non-zero.
  **Against synth:** at the default tune, `Rig::build` agrees with synth's own composition (4.1
  rule 2, 7.1) on every value of the small enums and on sampled `EulerConv`s: `lens_from_rig` is
  the inverse of synth's `to_rig` within 1e-12, the models within 1e-9 px and the centres within
  1e-12 m.
  The rig properties prove self-consistency; this and 7.1's hand-worked values tie both to 6.2's
  text.
- **Weights** lie in [0, 1], sum to 1 where a lens covers, are continuous, and are symmetric under a
  lens swap with a mirrored rig. **Distance:** a point at R seen with q = 1/R puts both lenses'
  rays through it within 1e-9 m, the error growing with |q − 1/R|.
- **Mask:** for rigs within validate's bounds (11.2), Pat's V6 numbers under map A among them
  (θ_frame 99.1° on lens 0 [C]), every direction with a non-zero weight has its footprint inside its
  slot by the margin, and `theta_frame` matches a scan of the edges at one sample per texel within
  0.01°. Control: the frame term off finds footprints outside on that rig.
- **Rung 0:** sphere's forward of synth's inverse is the identity on 10k seeded points per truth
  model, at the same tolerances.

## 7. The synthetic camera and its files (`synth::lens`, `synth::insv`)

### 7.1 `synth::lens`

```rust
pub enum TruthModel { Mei { xi: f64, fx: f64, fy: f64, cx: f64, cy: f64, k: [f64; 3], p: [f64; 2] },
                      MeiV6 { xi: f64, fx: f64, fy: f64, cx: f64, cy: f64, d: [f64; 13], reading: TruthV6 },
                      Kb { base: KbBase, f: f64, cx: f64, cy: f64, k: [f64; 5] } }
pub struct LensTruth { pub model: TruthModel, pub to_rig: DQuat, pub centre_m: DVec3,
                       pub theta_max: f64, pub side: u32, pub gain: [f64; 3], pub vignette: [f64; 3] }
pub struct RigTruth { pub lenses: [LensTruth; 2], pub scene_radius_m: Option<f64>, pub motion: Motion }
pub fn render_lens(pattern: Pattern, rig: &RigTruth, lens: usize, frame: u32, ss: u32) -> RgbImage;
pub fn lens_planes(img: &RgbImage, coding: Coding /* X5: full range, BT.709, chroma left */) -> Yuv420;
// synth::code (M3.1, for the perf clips): corner_code(img, lens, frame), read_corner_code(planes)
```

**Per pixel**, supersampled: synth's own inverse (a damped 2-D Newton on the full forward with a
numeric Jacobian, bisection as fallback; deliberately not sphere's bracketed radial Newton), the
closed-form lift, `to_rig`; for a finite scene the ray from `centre_m` meets the sphere of radius
R; then `color(pattern, d, frame)`, the gain in linear light, the sRGB OETF. Past `theta_max` or
with no lift: black, as on the camera. The X5-like truth's `theta_max` is 106°, where the X5's
image circle ends (understand-format.md §3), so its square frame cuts the circle on all four sides
as the camera's does: r(106°) = 395 px against a half side of 384 at 768² [C], while the rig's
θ_max stays 100°. `dfish_mask_margin` sets the truth's `theta_max` to the rig's, so the black ring
sits where the mask must hold it off. **Frame codes.** The circle leaves the square's corners
free, so a block of side b = s/12 in each corner (32 px at 384², 64 px at 768², 320 px at 3840²)
holds 16 bits of frame number, the lens index and a checksum, read back from decoded or uploaded
planes: pairing is checked at the pixel. synth asserts
b ≤ s/2 − |centre offset| − (r(`theta_max`) + 2)/√2 for every truth it renders, so no block
reaches content: for the X5-like truth with centres up to 3 px off, the bound is 47.8 px at 384²
and 100 px at 768² [C]. A 64-px block at 384² would reach θ ≈ 94°, inside the feather and the
strips [C]. `Grid` gains a back-hemisphere frame code.
**Extrinsics, independently** (4.1 rule 2). `synth::lens::compose(&CalibTruth, &Planted,
front_track) -> RigTruth` builds each lens's model in its slot's pixels under the planted canvas
map, its `to_rig` and its `centre_m` from the stored triples, the roll offset, the binding, T_b, S,
U and c_1 with the tz sign. It is written from 6.2's text with glam's axis-angle quaternions, never
through `sphere::rig`; `Planted` and `TruthV6` (6.1's space) are synth's own types. Its unit tests
check values worked out by hand, with the arithmetic in their comments: 8 `EulerConv` values, the
placeholder (identity with the −90° roll offset) among them; block 1's V6 centre and focal (cx
8075.73, so c_local 2699.73; fx 4284.40) under A, B and C, where at 3840² A and C give cx 1928.38
and B 1928.48, A and B give f 3097.16 and C 3060.29, and at 768² cx is 385.68 under A and C and
385.70 under B [C]; and c_1 for Pat's V6 t_1 = (0.637, 0.001, −31.525) mm under both tz signs.
6.3 compares `Rig::build` with it.
**Cost:** 768² × 2 lenses at ss = 2 (2 × 2 samples a pixel, as `render_view_ideal`'s n × n,
`synth/src/lib.rs:336-339`) is 4.7 M samples, 3.6 M inside the circles, about 2 s on 4 threads [G].
Each test binary keeps a cache of renders keyed by (rig truth, scene, frame): a static scene is
rendered once, and its frames differ only by their frame codes and the noise added after
rendering. For the fit's checks (8.5), `RigTruth` also takes a ground plane at a height below the
rig, a time offset per lens in frames and a rolling-shutter readout time (each row rendered at its
own time).

### 7.2 `synth::insv`, the LRV and the perf clips

```rust
pub struct InsvSpec { pub rig: RigTruth, pub pattern: Pattern, pub frames: u32, pub rate: FrameRate,
                      pub quality: Quality /* Lossless | CameraLike { crf } */,
                      pub calib: Planted /* synth's own enums (7.1): binding, canvas map, Euler, turn, upright, tz, V6 */,
                      pub records: Records, pub fault: Option<InsvFault>, pub lrv: Option<LrvSpec> }
pub enum InsvFault { TruncatedTrailer, IndexSlotPastEnd, BadMagic, WrongTokenCount, VarintOverflow,
                     LensOneFrameShort, StartPtsDiffer, StssDiffer, RateDiffer, NoCalibration }
pub fn write_insv(spec: &InsvSpec, dir: &Path) -> Result<InsvFiles, SynthError>; // master, LRV?
pub fn print(cal: &CalibTruth, planted: &Planted, generation: Generation) -> String; // from numbers (M3.2);
                                                                                  // RigTruth through `compose` (7.1, M3.3)
```

**The container** has two square `hvc1` tracks with the X5's coding (full range, BT.709, chroma
left, the mode's GOP as 3.1 gives it, no B-frames; insta360.md:33-37), one silent AAC track, `ftyp wide mdat moov`, then
`inst` of size E + 8 and the trailer; `Mp4Writer` takes N video streams (`encode.rs:544-569`), and
`Quality::Lossless` is libx265 `lossless=1`, which every FFmpeg we build against has. **The
trailer** comes from synth's own writer, not camera's (insta360.md §3): 6-byte footers on 128 KiB
boundaries, the 31-slot index at EOF−388, the 72-byte tail and magic, record 1 as hand-written
varints, records 3 and 4, and optionally record 2 (a 1280×640 NV12 equirect rendered by synth, for
8.8's check). A reader bug that a shared writer mirrored would go unseen. **The strings** are
printed under the planted convention from the format notes' numbers (§4.2) scaled to the lens
size; the serial is `SYNTH00001`, never Pat's. **The LRV** is one H.264 1664×832 track, track 1
on the left [F], with the X5's frame-count rule and the master's trailer. **Sizes:** the rung-3
file is 2×768², 8 frames, lossless; rungs 4 and 5 (the export and the viewer) use that
file with 11.5's tune; the other app and agent tests use 2×384², 30 frames, against their own
blessed values. **Perf clips** (M3.1) for hw-test: 2×3840² at 29.97
and 2×2880² at 29.97 and 59.94, from `perf_clips::ensure`'s two-track specs. Each lens is coded on
its own, threaded x265 with its corner codes and its own noise seed, and the two go into one MP4:
`mux_video_streams` gains an MP4 output that keeps each track's time base (its Matroska output
rounds to the millisecond, `encode.rs:1305-1311`), so `track_timing` reads the clips and the lock
opens them. The same writer makes section 9's 2×64² test files, with a length and a GOP per track.
The coding follows the X5's in each mode, from `ffprobe` and the NAL types of the 7 masters [F]:
closed GOPs, no B-frames, one-pass ABR, noise on, and hw-test's "too easy" flag checked per track.
3840² at 29.97 has an IDR every 60 frames and P frames only, at 65 Mbps a track (the masters: 61
to 68). 2880² at 59.94 has an IDR every 120 frames and every other frame a non-reference picture,
as the X5's TSA_N, at 80 Mbps (79). 2880² at 29.97, which no sample has until take 16, has an IDR
every 60 frames and P only, at 80 Mbps [G]. Today's perf clips are open GOPs of 30 with 3 B-frames
(`synth/src/media.rs:875-902`), which halve an exact seek's decode against the camera's. Whether
the x265 we link can mark every other P frame non-reference is checked on its output's NAL types
[G]; if it can't, the 59.94 clip is P only and its rows say so. From M3.10 `perf_clips::ensure`
also appends synth's trailer (M3.2's writer, an X5-like rig's strings, `SYNTH00001`) to the
2×2880² and 2×3840² clips, so the stitched benches and `bench_export_4k30_from_insv` open them as
`.insv` files through the camera path; their picture is noise, which costs the stitch the same as
a lens image.

## 8. Measuring the seam (`render::compare`, `render::seam`)

One implementation in render, which already builds at opt-level 2 in dev, serves the round trip,
the goldens, the fit, Tier 2, the app's Measure seam and agents' `look seam`.

### 8.1 Masked metrics

```rust
pub struct Weights<'a> { pub width: usize, pub height: usize, pub w: &'a [f64] }
pub fn psnr_weighted<A: Sample, B: Sample>(a: View<A>, b: View<B>, w: &Weights) -> Result<f64, Mismatch>;
pub fn ssim_weighted<A: Sample, B: Sample>(a: View<A>, b: View<B>, w: &Weights) -> Result<f64, Mismatch>;
pub fn diff_stats_weighted<A: Sample, B: Sample>(a: View<A>, b: View<B>, w: &Weights, lsb: f64) -> Result<DiffStats, Mismatch>;
pub fn equirect_coslat(width: usize, height: usize) -> Vec<f64>;
```

The weighted SSIM averages the SSIM map over window centres with their weights. Masks come from
`render::seam::band_weights(rig, view, size, inner_deg, outer_deg)` and `lens_coverage(rig, b,
view, size)`, per output pixel in f64. **Self-tests:** all-ones weights reproduce `psnr` and `ssim`
exactly; an error planted at weight 0 changes nothing, and at weight 1 lowers the PSNR by the
computed amount.

### 8.2 The seam signature

**Strips.** In the rig frame the seam is the plane z = 0. A strip pixel (φ, ε) is the direction
d = (cos ε cos φ, cos ε sin φ, −sin ε): φ runs around the seam (0 right, 90° the zenith, 270° the
nadir), ε is the angle off the seam towards the front lens. `strips(rig, [planes; 2], spec)` renders
the band |ε| ≤ 8° twice, each lens alone with its parallax-corrected ray, through the CPU
reference's trilinear footprint. The band stays inside the X5's ±10° overlap with room for the
mask margin. Pixel scale: in Tier 1 the source's own scale at the seam (3.81 px/° across at 768²);
in Tier 2, 8 px/°, with shifts reported in source pixels.

**Shifts per bin.** `bin_shifts(&strips, spec) -> Vec<BinShift>` works in 72 bins of 5° in φ. Each
bin finds a coarse shift by NCC over ±3° (enough for the 2.2° focal control), refines it with
three Lucas–Kanade steps on patches normalised for gain and bias, and keeps the 2×2 normal matrix
as the shift's information, so texture along a single line (a horizon, a fence) gives
information across the line and none along it, rather than a false shift (the aperture problem).
Samples above 0.95 or below 0.01 are left out, and a per-channel median log ratio is kept for the
photometric part.

```rust
pub struct BinShift { pub phi_deg: f64, pub across_deg: f64, pub along_deg: f64,
                      pub info: [[f64; 2]; 2], pub ncc: f64, pub log_gain: [f64; 3], pub valid: bool }
```

**The signature.** `signature(&bins) -> Signature` is a least-squares fit weighted by each bin's
information: across(φ) = **a0** + a1 cos φ + b1 sin φ + a2 cos 2φ + b2 sin 2φ and along(φ) = **c0**
+ c1 cos φ + d1 sin φ, plus the rms of what is left, the number of valid bins, the median NCC and
the photometric log gains. On near scenes it can add a q per 15° sector, which soaks up depth that
varies with azimuth and, with it, 93 % of a relative tilt (8.4).

| Term | What produces it | What tells the causes apart |
|---|---|---|
| a0 (across, uniform) | focal scale (Q4); uniform-depth parallax (q, and with it the tz sign); distortion modelled wrong in θ (radially symmetric, so uniform across at θ ≈ 90°) | parallax ∝ q, scale is the same at every depth: take 14 (q ≈ 0) isolates scale; with the scale fixed, clip 8's q and its sign give tz (8.6) |
| a1, b1 (across, first harmonic) | relative tilt; centre offsets; the Euler class; V3 against V6; V6's tangential and prism terms (Q5); the binding reversed or the tracks swapped (a tilt of twice the lenses' angle difference along the turn axis, 3.4); a lens late under a pan or tilt | c1/d1: a centre offset moves points along as well as across, 90° out of phase, a tilt only across (3.3); the rest by scoring whole candidates with the stored centres held. Equal offsets of both centres are a pure tilt, so only the centres' difference can be told from one (8.4); a lens late scales with the frame's turn rate (4.3) |
| a2, b2 (across, second harmonic) | fx ≠ fy | the residual between candidate readings |
| c0 (along, uniform) | relative roll; a lens late under a turn about the lens axis | a lens late scales with the frame's turn rate (4.3); a roll does not |
| c1, d1 (along, first harmonic) | the centres' difference; the binding's swapped centres; Q5's tangential and prism terms (most of their 12 to 33 px, 3.4) | their predicted fields per candidate |
| rest rms | depth varying with azimuth; the non-rigid part of rolling shutter | the per-sector q; rolling shutter scales with the turn rate, so stage F uses frames at rest (8.4) |
| median NCC < 0.5 | the turn wrong or left out; a gross Euler error. The tracks swapped do not collapse it: they are a common turn plus a tilt of up to 2.4°, inside the ±3° search (a1/b1 row) | gross failure: NCC collapses |
| log gains | exposure and white balance | geometry is invariant to gain, since patches are normalised |

Brown's p1 and p2 at radius r give a first-harmonic field (3r²(p1 sin φ + p2 cos φ) across, r²(p1
cos φ − p2 sin φ) along), and V6's p′ and s terms do too [C]. So Q5 lands on the first harmonic, as a
centre offset would: within a band of ±8° the r² scaling barely changes. But 8.4 judges every
reading with the stored centres held, and then the readings stay far apart: against `RadialOnly`,
the `V6Reading` values {D6D5 with extension, D9 Interleaved}, {D5D6 with extension, D9 ByAxis} and {D5D6, D7 ByAxis} (6.1) move a seam pixel by up to 15.5, 33.0 and 20.1 px on lens 0 and 11.9,
20.5 and 17.5 px on lens 1 at 3840², 12 to 32 px of it along the seam, and they differ from each
other by 20 to 36 px [C]. Parallax never moves points along the
seam, so clips 8 and 10 can already rank them (8.6). "Not measurable" is kept for readings whose
predicted fields differ by less than the floor.

### 8.3 The attribution matrix and the instrument's own tests

`seam_signature_attributes_planted_errors` (Tier 1). Each geometric and photometric control of
4.2 is planted. Most need no new images: the error goes into the model used to unwrap one rendered
pair; depth, motion and gains need re-rendered pairs. For each control: (1) **predict** the shift
field point by point in f64, each seam sample through the truth's inverse and the model's forward,
with no images; (2) **measure**: the image-based estimator must match the prediction within 0.05 px
rms; (3) **attribute**: the dominant term must be the one step 1 predicts, and every other term must
stay under max(0.03°, 15 % of it), except the pairs the geometry couples (a centre offset, and with
it the tracks swapped, lands on a1/b1 and c1/d1 together, with the predicted ratio). The predicted
term is not always the obvious one: lens 1 a frame late while the rig pans at 10°/s about +Y is a
rigid 0.33° turn about +Y and lands on a1; the tracks swapped land on a1/b1 and c1/d1, never on c0
or an NCC collapse (3.4). This one test proves that the
instrument measures what the geometry predicts, that the terms separate where they can, and that
each control is detectable. Its renders come from the binary's cache (7.1), and it is costed with rung 1 in 19.6.

**Self-tests:** a pure shift planted by resampling one strip comes back within 0.02 px; swapping
the strips flips every term's sign; gains on a strip leave the geometric terms under 0.005°; a
textureless bin is marked invalid, never zero. The round trip proves self-consistency only, and
says so wherever it is cited; only rung 8 speaks for the real camera.

### 8.4 The fit (`render::seam::fit`)

**Candidates** are `Geometry` values (6.2). **Stage G, gross,** on 480² luma, through `sphere::rig`
directly with bilinear samples: every binding × turn axis × roll offset × `EulerConv` without the
roll offset (2 × 2 × 4 × 288 = 4608), less those whose lens_from_rig_0 without S (6.2) puts block 0's lens more than 5° from −Z, so the filter is the same under either binding and either `front_track` (2304 remain with Pat's
angles; each one dropped is an exact twin of a kept one or does not put the lenses back to back, except 256 that are back to back with lens 0 sideways and predict relative rotations 0.27° from every kept one,
6.2 [C]), scored by the band NCC over about 20,000 samples per frame,
under V6 `RadialOnly` (480² cannot see V3 against V6). Candidates within 0.8 × the best score
survive. About a minute on 4 cores in release [G]. Binding is searched jointly with the Euler
convention because under some readings swapping the blocks' rolls is a common rotation the seam
cannot see, so deciding it first could lock in the wrong one.

**Stage F, fine,** at full resolution through the CPU reference strips and the signature, for each
surviving class and for V3 and each V6 reading, on frames at rest only: frames in which lens 0
turns by less than 0.02° from one frame to the next, as the signature of lens 0's strip against
itself a frame later measures it (M3 does not read record 3). A lens late by Δt under a turn ω is
a rigid relative rotation ωΔt (3.4), so on moving frames a sub-frame offset or rolling shutter
would bias the rotation. Free parameters: the relative rotation (3), a common focal scale (1), the
relative centre (2; lens 0's centre is held as the gauge: the centres' difference shows along the
seam, and their sum is indistinguishable from a relative tilt, at 0.0375° per canvas px [C]) and q,
bounded by the take. On take 14, q is the ground under the tripod as a plane, q(φ) = max(0, −sin e)/h
for a seam point at elevation e, measured from the level the horizon gives rather than from the rig's own axes (a 5° roll taken as level, which the usable-take rule allows, puts 0.05° on a1, the whole of stage J's tolerance [C]), with h Pat's tripod height (SAMPLES.md take 14,
`samples-roles.toml`) and the far field at q = 0, since a uniform q is the scale's a0; h is also
freed as a check. Left unmodelled, that ground puts 0.4° on a0 and −0.6° (12 px at 3840²) on b1
with 30° to 50° around the nadir left out, and still −0.23° (4.5 px) with 81° left out [C]. On clips 8 and 10, q is
free in [−1, 3] m⁻¹ per 15° sector, with the scale fixed. Every take leaves out the bins within 30° of the nadir (the tripod). Where the nadir lies, for these bins and for the ground model, comes from the up-and-down rule (8.6), run on stage G's best class before stage F: twins differ by a common quarter turn about Z (6.2), so until `upright` is set a member's own rig frame can put φ = 270° a quarter or half turn from the tripod. Levenberg–Marquardt with a Huber loss at 0.5 px, hand-written: a 6×6
solve on take 14 (7×7 with h), and on clips 8 and 10 a 5×5 solve with the 20 sector q that keep bins profiled out (24 less the 4 within 30° of the nadir)
(each is linear in its bins' across shifts, so it is solved in closed form at every step). Stage F
returns its information matrix (JᵀWJ, after the profiling), each parameter's standard error from
it, after dividing JᵀWJ by the weighted residuals' mean square (each bin's normal matrix gives its information only up to the image noise's variance, so without it the standard error is not in degrees), and the spread over 5 disjoint frame sets; σ is the larger (the noise floor below). A direction
of the relative rotation whose σ exceeds the spacing of the competing predictions along it is
reported as undetermined. Wherever q is free per sector (clips 8 and 10, and 8.5's R = 1.5 m case), both tilt directions are undetermined by rule, whatever σ says, and a `Fitted` outcome there keeps the best class's predicted tilt: σ measures noise, not the sector model's bias. A plane wall 1.5 m to one side, whose q varies inside each sector, puts 0.6° on the profiled tilt, while the tilt's σ is 0.006° to 0.07° for 0.05 to 0.5 px of noise a bin [C]. The sector q also absorb 93 % of a
0.3° tilt and leave 0.015° (0.3 px at 3840²), and the tilt's standard error is 12 to 17 times what
it is with q held [C]. **Binding, V3 against V6 and the V6
reading are judged with the stored intrinsics and centres held**, in stage G and J, never on stage
F's residuals: F frees exactly the parameters a binding swap exchanges, so its residuals would
come out nearly equal.

**Stage J, judge,** on a stage-F fit with both centres held at their stored values and only the
rotation free (with the scale and q as above): the stored angles and centres come from one
calibration, and a held lens 0 centre that is wrong by 1.3 canvas px would otherwise use up the
whole 0.05° [C]. Each `EulerConv` in a surviving class predicts a relative rotation from the two
stored triples; those within max(0.05°, 3σ) of stage F's in every direction stage F determines
form one **equivalence class**, and the report names the undetermined directions. Members that
also agree within 0.05° in the undetermined directions render the same; a class whose members differ there
settles only what they share (8.6, Q6). Twins (6.2) are one class, and the class is what is
reported. If none predicts it, the
outcome is `Extrinsics::Fitted` with `Status::Fitted`, so a convention outside the space lands as
a measured rotation, never as a wrong "settled" value.

**The decisiveness rule.** A question is settled when all of these hold:
1. the best class's residual is ≤ 3σ plus the parallax allowance (0.18° on take 14);
2. the runner-up class's residual is ≥ max(2 × best, best + 5σ, best + 0.3 px at 3840²);
3. the same class wins on at least 4 of 5 disjoint frame sets;
4. the same class wins on a second scene where stage F's information matrix determines the
   question: for clip 8, clip 10's frames at rest after a turn, or take 14; for take 14, its second
   heading (question 1), or clip 10 where it determines the question. Frames of one tripod take differ only by sensor and codec
   noise, so condition 3 alone is not independent evidence; condition 4 is.

Otherwise the question stays open, with its numbers, in the slice note and insta360.md. **The
noise floor** σ of each term or parameter is the larger of its standard deviation over the 5
disjoint frame sets and its standard error from stage F's information matrix, and never under
0.05 px at 3840² (0.0026° across, 0.0017° along). The frame sets take 10 frames spread over
different GOPs, keyframes and mid-GOP frames alike (`--frames 0:300:30` spans five 60-frame GOPs),
of a still tripod take (take 14; clip 8 until then), and in Tier 1 of a camera-like synthetic
`.insv` coded with an IDR every 2 frames, so that its 10 frames a geometry (19.6) span five GOPs, a keyframe and a P frame in each; at the 60-frame GOP 7.2 would give it, all 10 would sit in one. Both figures measure noise, not the bias that scene content gives the estimator, so the
report puts each term's spread between clips 8, 10 and 14 beside σ.

**Refinement.** With the convention fixed, the stage-F deltas (rotation, scale, relative centre)
are what the calibration would need. On take 14 they should be within the floor. They are never
applied silently: they become a suggested tune that Pat or an agent may apply, or, on a redirect,
the unit's own calibration (8.7).

### 8.5 Validated before it is trusted

`fit_recovers_planted_geometry` (Tier 1, render) renders, through `synth::lens` at 768² with
camera-like noise and libx265 at CRF 23, the X5-like rig on `SeamLines` under 2 seeded planted
geometries: one at R = ∞ with a ground plane 1.5 m below the rig, one at R = 1.5 m. The candidates
are the planted one, its hard neighbours (the binding swapped, A against C, the other V6 readings,
its twins, and Euler classes that differ only in the small angles, among them one that differs
only in tilt and one that differs in relative roll) and 24 random ones. Each expected verdict below
is a `must_fail` control on the rule, so a rule that always settles, or never does, fails:

- **R = ∞:** the planted class, the binding, the reading and the canvas map win under conditions 1
  to 3 of the decisiveness rule, with the scale within 0.1 %. With the ground model switched off,
  b1 carries the ground's bias (about 0.6°, 12 px at 3840² [C]) and the planted class no longer
  settles.
- **R = 1.5 m, q free per sector:** the gross class, the relative roll, the binding and the reading
  win and the sectors' median q is within 10 % of 1/1.5 m; the tilt-only neighbour stays in the
  planted class's equivalence class (the fine class open), and the roll neighbour is separated.
- **f × 1.012:** Q4 picks the other reading. **Lens 1 yawed 0.5°**, with no candidate predicting
  within 0.3° of the result (the test checks): `Extrinsics::Fitted` with `Status::Fitted` and the
  0.5° in the refinement, never a settled class.

It covers conditions 1 to 3, the `Extrinsics::Fitted` branch and Q4's settled branch, in about
15 s (19.6); condition 4 and the other redirect branches are `fit-matrix`'s. `cargo xtask
fit-matrix` (release, about 15 minutes on 4 cores [G]) runs the 6 seeded geometries over the whole
space, each at R = ∞ in two headings (the scene turned 90° about +Y, as take 14's second heading)
and at R = 1.5 m, so condition 4 is exercised, and these truths with their expected verdicts: a
relative rotation 0.3° from every prediction gives `Extrinsics::Fitted`; a scale of 1.006 at R = ∞
gives `CanvasMap::Fitted`; d5 to d12 = 0 gives Q5's "not measurable"; a binding swap planted where
it is a common rotation (the blocks equal but for their rolls, the roll about the lens axis) gives
"indistinguishable"; a KB-rendered lens fitted as Mei, with vignetting, leaves no class settled or
takes the residual-floor redirect; lens 1 half a frame late with a 21 ms rolling-shutter readout,
the rig panning at 10°/s between rests, keeps the planted verdict on the frames at rest, and on the
moving frames comes out open or reports the half frame within 0.1 frame (4.3's regression). Its
report goes in the note of any slice that touches the fit, and it is part of the push rule (4.1
rule 6) for any slice that touches `render::seam`, `sphere::rig` or `synth::lens`.

### 8.6 The rules for Pat's takes, fixed before they arrive

Residuals are in px at 3840² (19.0 px/° across, 30.0 along). σ is from 8.4.

| Question | Data | Settled when | Redirect when |
|---|---|---|---|
| Usable take | take 14 | ≥ 12 of the 18 bins within ±45° of each side (φ = 0° and 180°) valid, with bins within 30° of the nadir (the tripod) left out and the ground modelled (8.4); the horizon within 5° of level; after the ground model, no 15° sector's across residual exceeds 0.18° (3.4 px, the parallax of something at 10 m); both headings present (question 1); Pat's note gives the tripod's height and says nothing stood nearer than 10 m | Pat gets a re-take request that says what was missing |
| Gross | take 14; clips 8 and 10 for M3.6's entry | the best stage-G NCC ≥ 0.6 | **kill**: M3.12 stops for a design note on the lens frame, and the real-seam gates wait; every other slice carries on with synthetic rigs and the provisional entry. On clips 8 and 10 (M3.6): the placeholder stays, with `stitchable: false` (5.3), and the design note starts then |
| Q2a, screen side | take 15 | one track's mean luma over 0 to 2 s is under 30 % of its mean over 3 to 10 s, and the other's is at least 80 % | neither or both: Pat redoes take 15 |
| Q2b, binding | clips 8 and 10, then take 14 | the decisiveness rule on the joint stage-G and J search, intrinsics held; on clips 8 and 10 through the swapped centres' along-seam part, since the binding's tilt is undetermined there (3.4) | indistinguishable: keep `Reversed`, with "indistinguishable at the seam" as evidence |
| Q4, scale | take 14, with 8.4's ground model and the far field at q = 0 | stage F's scale is within max(0.25 %, 3σ) of one reading (C = 1; A and B = 5376/5312 = 1.01205) and ≥ 5σ from the other, with the correlation between scale and tilt in stage F's covariance under 0.9 in size and the fitted tripod height within 10 % of Pat's. The 0.25 % covers the parallax left beyond 10 m (0.18°, about 0.10 % [C], which biases the fitted scale high, towards A) | between or beyond: `CanvasMap::Fitted {scale}`, kept in this unit's profile (8.7) |
| Q6, Euler class, turn axis, roll offset | clips 8 and 10 for the gross class and the relative roll; take 14 for the fine class (the tilt) | the decisiveness rule holds for one class, judged in the directions stage F determines (8.4), and every stage-G candidate whose predicted relative rotation lies more than 10° from the best's scores at most half the best NCC (with Pat's V6 angles the kept candidates' predictions, both bindings together, lie within 2.49° of each other or 178.7° or more apart, so a 2° cut would put fine neighbours 2.0° to 2.49° away, 5 to 9 px at 480², under the halving test [C]); nearer ones, twins included (6.2), are stage J's. The fine class settles only where its members agree within 0.05° in every direction, so on clips 8 and 10 only the gross class and the relative roll can | none: `Extrinsics::Fitted`. Several (twins, or members that differ only where the take determines nothing): the simplest member, with `upright` from the up-and-down rule, and the class and its open directions recorded |
| Q5, V6 reading | clips 8 and 10 on the along-seam terms (centres held, the rotation's c0 free), then take 14 | the best `V6Reading` value lowers the median residual against `RadialOnly` by ≥ 0.3 px and wins over the other 20 under the decisiveness rule (other values beat `RadialOnly` too: 2 to 9 of them for each sourced reading on Pat's V6 numbers [C]) | no listed reading fits (the best one's residual is above the residual floor): the best reading, recorded as "no listed reading fits, residual N px", and the residual-floor branch; "d5 to d12 not measurable at the seam on this unit", with `RadialOnly`, only when the readings' predicted fields differ by less than the floor |
| V3 or V6 | take 14, centres held at the stored values | one is lower by ≥ 0.3 px under the decisiveness rule | V6, the one field 136 = 4 marks in use |
| tz sign | clip 8, after Q4 | with the scale fixed, the sign under which the sectors' median q lies in [0.4, 1.0] m⁻¹ (the wall at about 1.5 m is 0.67); the other sign gives q < 0 | neither: the translation is fitted. Before Q4 is settled: `AsStored`, the physical prior |
| Residual floor | take 14 | the best median residual is ≤ 1.5 px (0.08°) | an own calibration that also frees each lens's scale and ξ (8.7) |
| Up and down | take 14, clip 10 frame 0 | exactly one of `upright`'s four quarter turns (6.2) puts the stitched horizon within 5° of the equator with the upper hemisphere's mean luma above the lower's, and that one is stored; and, if Pat agrees (question 2), record 2's thumbnail registers within 1° of identity under it | none or several: `upright` stays 0, recorded as open; a disagreement between the two checks goes into the slice note |

**Before takes 14 and 15 arrive,** M3.6 runs the same rules on clips 8 (the wall), 9 (handheld,
fast turns, for the lens-offset test) and 10 (tripod, slow turns), and records the result as
provisional evidence. The X5 entry takes from that run, where decisive, its binding, its V6
reading and its gross class: the turn axis, the roll offset and the Euler class up to twins and to
the tilt directions clips 8 and 10 cannot determine (8.4). Within the gross class it uses the
simplest member that fits the relative roll, with `upright` from the horizon rule, and records the
fine class as open until take 14. Q4 cannot be settled there and stays C [G].

**M3.6 always writes an entry.** Where a rule is not decisive on clips 8 and 10, the entry takes
that row's redirect value, or the best stage-G and F candidate where the redirect needs one,
with evidence such as "best of N, not decisive". 5.3's identity placeholder outlives M3.6 only
when the gross rule fails there.

### 8.7 The redirect: an own calibration

If a rule redirects, the fit's numbers become data: `CanvasMap::Fitted`, `Extrinsics::Fitted`,
and, on the residual-floor branch, a scale and ξ for each lens (`radius_scale` and a ξ delta in the
tune). They live in the camera's profile (17); projects copy them with `Status::Fitted`, and the
evidence names the take and the report. The renderer, the schema and the pane don't change. A
Studio export is never fitted to.

### 8.8 Outside checks

- **Record 2's thumbnail** is the camera's own 1280×640 stitched equirect (insta360.md:58-61,
  [reported] for the X5; M3.2's reader decodes it and checks its layout). A rotation fit between
  our stitch at frame 0 and it says which way the camera calls up and forward, and catches the turn
  axis and gross Euler errors at about 0.2° [G]. It is pixels the camera wrote into Pat's file, not
  vendor code, but it is the vendor's stitch, so its use is Pat's call (question 2); it checks and
  is never fitted to, and the horizon-and-sky rule (8.6) settles up and down without it.
- **Take 15:** the track whose mean luma falls is the screen-side lens. That settles the physical
  side for the default facing and the pane's names, not the binding, which the fit settles.
- **Handedness:** every lens frame has det +1 by construction, and the round trip's frame codes
  read wrong if mirrored; on Pat's footage, the text on clip 8's wall must read left to right.

### 8.9 The `rig-fit` tool

`cargo run -p spherecut-cli --features fit --bin rig-fit --release -- <insv> --frames 0:300:30
--kind open|wall|turn|cover --json out.json` is a feature-gated developer tool, as timeline-proto
was (D53). It decodes in software through `Still::open_pair`, builds candidates through camera,
calls `render::seam::fit`, and prints the ranking, the classes, σ, the directions stage F leaves undetermined, the rules that fired and the
refinement; each take's report goes in `docs/research/m3/fit-<take>.md` with its command. It runs
in the cloud (Tier 2c) in minutes.

## 9. Frame-locked pairs (`playback`, `media`)

Today the lenses agree by luck: `pick` takes each stream's latest Ready slot on its own, a stream
short of slots shows its frame as it arrives, catch-up is per worker, `check_end` looks at stream
0, and nothing checks that two keyframe landings match (section 2). An all-or-none present on top
of those parts would freeze the view whenever one lens lags (the group base, below). The lock group
keeps three promises: a pair is never mixed; a time both lenses hold always exists, so the view
goes on; and memory stays bounded when one lens is slower than the other:

- **Open.** `Player::open_locked(path, streams: [usize; 2], lock: LockSpec)` sits beside
  `open_streams`. The timing comes through the backend, so tests can plant it: a new trait method,
  `MediaBackend::track_timing(path, stream) -> Result<Option<TrackTiming>, MediaError>`, `Ok(None)`
  for a track with no tables. It has no default body, so the compiler finds every backend that must
  answer it. Every backend that wraps another passes it on (`GatedBackend`, `synth/src/fake.rs:601`;
  the `FaultBackend`s at `engine.rs:1499`, `app/tests/panes.rs:295` and `synth/tests/fallback.rs:100`;
  `SlowSecond`, `Slowed`, `Held` and `LavapipeBackend` in `engine.rs`) [F], and `SyntheticBackend`
  answers from its clip's rate, frames and keyint (`fake.rs:39-46`). A wrapper that answered `None`
  instead would refuse every lock with `"timing"` unseen: the `FaultBackend` test below would fail, and
  `never_blocks` (13.5) would pass with no lock formed. `FfmpegBackend` answers from P-TRK's `media::mp4::track_timing` for an MP4 track and
  `None` for any other container; the test backend of the Tests below answers with planted tables.
  `OpenJob::run` (`worker.rs:105`) reads it for both streams whatever the decoder preference (its
  probe, `worker.rs:227-232`, runs only under Auto, so it can't be the source [F]), and each worker
  adds its decoder's `VideoStreamInfo` once open. The pair locks if they agree on time base, start
  pts (after `LockSpec.offset`), constant rate, size, and `stts` and `stss` over the common prefix
  (`stss` is left out if P-TRK takes R2, where the later-landing seek below covers it). The frame
  count is not compared: a lens with fewer frames ends the pair (below). If they agree, a
  `LockGroup {members, until}` goes into `Shared`; if not, `PlayerEvent::LockRefused {field}`
  (`"timing"` for a track with no tables), and the app shows the raw view with a notice naming it.
  The presenter shows nothing of the pair until the group has formed or been refused.
- **Present.** `pick_pair(views, members, epoch, t) -> Option<[SlotId; 2]>` is a pure function: the
  largest T ≤ t at which every member has a Ready or Shown slot at exactly T, in clip time, with t
  the clock's time. A locked group shows all of its slots or none (if the second `try_show` fails,
  the first is given back) and never shows a frame as it arrives: that branch
  (`player.rs:1092-1101`) stays for unlocked streams, and the slot floor below keeps a locked pair
  from needing it. `PerfStats.unpaired_presents` counts presents at which the player's streams show
  different clip times, over every stream it shows, locked or not, so the lock-off control counts
  on the same terms. It goes into the overlay and the bench JSON beside `pair_wait` (how far the
  pair shown trails the clock) and `pair_resyncs` (below). It compares the slots' time tags, so it
  can't see a slot whose texture and tag disagree (a slot reused before its upload lands, a path-B
  image recycled); the tests read the textures back for that.
- **The group base.** Upload is per stream today: each stream's wanted list starts at its own
  frame at the playhead (`upload.rs:410-420`, `456-461`), every Ready slot not in it is freed
  (`upload.rs:366-375`), and the cache evicts the frame farthest behind the playhead first
  (`cache.rs:37-41`) [F]. So when lens 1 falls behind the clock, lens 0's frame at lens 1's time is
  freed or evicted, no common T newer than the pair on screen exists, and an all-or-none present
  would hold that pair while the clock and the audio run on. The lock moves the cache's playhead
  instead. The cache learns the group at open (`FrameCache::set_group(members)`) and keeps the
  time the presenter gives it, as today (`player.rs:1172-1179`, `cache.rs:684-701`). From that
  time t it derives, under the lock it already takes, the **group base** Tg: the largest T ≤ t at
  which every member holds a frame, worked out again whenever a frame goes in or t moves, so a
  paused pair still reaches the clock's frame. From a seek or a step (each worker's `set_playhead`,
  `worker.rs:503, 541`) until every member has landed or failed, and whenever no common time
  exists, Tg is t, and the pinned landings carry the pair. An older common time would deadlock a
  seek forward from play: a seek inserts its landing, waiting for room, before `land` pins it
  (`worker.rs:514-520`), so a lens holding frames past that time finds its landing past its window
  (`cache.rs:268-284, 395`) and waits, while the other lens lands and then waits with its next
  frame [F]; once both land, a common time can't vanish, since
  each member's frame at Tg is its current frame. The cache's rules (each stream's current frame,
  its window, eviction and back-pressure) and `FrameCache::playhead()`, which the upload thread
  starts each wanted list from (`upload.rs:345`), run from Tg instead of t. So each member's frame
  at Tg is never evicted, the faster lens decodes at most its window past Tg (held back to the
  slower one, with memory as bounded as today), and each member's Ready slots from Tg on stay in
  its wanted list. The upload loop goes by rank across the streams (`upload.rs:382-393`), so both
  frames of a pair go up before either member's next.
- **Catch-up.** One `until` per group, in `LockGroup`, and no member uses its own: today each
  worker retargets at its own instant (`worker.rs:716-743`), so two lenses would skip to different
  frames. A member whose newest frame is more than 2 frames behind the clock raises the group's
  `until` to the clock's time, but never past the newest frame that every other member holds;
  every member whose newest frame is before `until` catches up to it (`backend.rs:336-344`). Since
  `until` is a time the other members hold already, the frame the trailing lens hands out next
  always pairs. A leading lens that also trails the clock decodes every frame until the other
  reaches it: the pair can't run ahead of its slowest lens anyway. **At 8K catch-up skips no
  decoding.** Every frame of the 8K30 and 8K25 masters but the IDRs is a TRAIL_R reference picture,
  with no TRAIL_N (clips 1 and 2, both lenses) [F], so catch-up passes frames over but still decodes
  each one, and saves only their copy and upload. A lens below real time stays below it, and the
  pair runs at its pace, behind the clock by its lag: `pair_wait` and the drop counters show it,
  and the notice of the fallback rule below offers the proxy. At 5.7K60 and 4K120 every other frame
  is a non-reference TSA_N (clips 3 and 4) [F], so there catch-up can halve the decode.
- **Seeks and the end.** Exact seeks land both lenses on the greatest pts ≤ T (D29). If keyframe
  landings differ, the group seeks both exactly to the later one, and the clock takes the group's
  landing, not stream 0's (`player.rs:1261`). A seek or a step clears the group's `until`.
  `check_end` takes the smallest end, so a lens with fewer frames ends the pair. The player says so
  once at open (`PlayerEvent::LensShort {stream, frames}`, from the two counts), and the app shows
  "lens B has N fewer frames". M3.11's export ends at the same frame with the same warning (12).
  Drops count once.
- **The backstop.** The rules above keep a common time ahead of Tg while both workers decode. One
  state they can't rule out is a budget that shrinks mid-play (the recheck, `upload.rs:209-218`)
  and evicts a frame the trailing lens was catching up to. So if every member's worker is waiting
  for room in the cache while no common time after Tg exists, the group seeks both lenses exactly
  to the trailing lens's newest frame, without moving the clock, and counts a `pair_resync`. An
  exact seek of both always gives a common frame, so the view goes on.
- **Slots.** A locked pair asks for `MAX_SLOTS`, 6 (177 MB at 8K), whatever the setting (3 to 6,
  `slots.rs:54-56`), or 8 after P-LOCK's redirect. It needs at least 2 × members live slots, 4 for
  a pair. With 3, the pair on screen holds 2 Shown slots, one lens's next frame takes the third,
  and the other lens's never gets one, since the lock never shows a lens alone: M1's two-stream
  deadlock, which the as-it-arrives branch fixed. Live slots are the setting less the dead ones
  (`slots.rs:184`), and a slot the GPU has no memory for dies (`SlotDropped`,
  `player.rs:149-157`) [F]. When a death takes a locked group below its floor, the group unlocks
  with `LockRefused {field: "slots"}`, the app shows the raw view with a notice naming GPU memory,
  and the as-it-arrives branch applies again; today's rule (no more live slots than streams means
  the GPU is failing) still holds below that. Cache keys stay (stream, clip time). Path B's
  two-lens pool may reach about 707 MB (understand-media.md §3), which the bench records.
- **Fallback.** A hardware failure on one lens moves only that lens to software, in place, through
  media's fallback wrapper (`fallback.rs:1-12`, D14); the other lens stays on hardware. Reopening
  both would gain nothing: every X5 sample decodes bit for bit the same on VA-API, Vulkan Video and
  software, two software 8K decoders together run at 0.42× real time, and the healthy lens would
  pay a software exact seek of 3.7 s median at 8K (perf.md) [F]. The pair then paces to the
  software lens by the rules above, and one notice offers the proxy. A member that can't go on (its
  open fails, or `DecodeFailed` after any fallback, `worker.rs:405-419, 828-846`) refuses the lock
  with field `"open"` if the group hasn't formed, and otherwise ends the pair at its last frame, as a
  short lens does. Today a stream's `end` is set only at `NextFrame::End` (`worker.rs:660-670`), so
  without this rule `check_end` never fires and `pick_pair` holds the last pair while the clock and
  the audio run on. A `PerStream` read error on lens 1 mid-play tests it: the pair ends within
  `WAIT`, and its control, the rule off, neither ends nor shows a new pair within `WAIT`.

**Tests.** Tier 1 (playback, lavapipe) runs on a new test backend, `PerStream`, in
`tests/engine.rs` beside `SlowSecond` (`engine.rs:686`). It holds one `SyntheticBackend` a stream
and opens the one `req.stream` names (`SyntheticBackend` ignores it, `synth/src/fake.rs:136-142`),
each with its own frame count, keyframe interval, start pts and frame cost. The cost is charged
for every frame decoded, handed out or passed over, as at 8K. Its `track_timing` answers each
stream's tables, derived from its clip or planted, so no file and no M3.5 fault is needed. The
clocks are `ManualClock`s, and the frame costs a few milliseconds.

- `pick_pair`'s property at 100k (random slot states, epochs, times and failed `try_show`s: never
  a mixed pair, always the largest common T).
- `a_locked_pair_never_shows_mismatched_frames`, beside `two_streams_play_on_three_slots…`: lens 1
  2.5 to 3× slower than lens 0 with jitter, 64 seeds of random seeks, plays, steps, pauses and
  rates. At every present both presented slots are read back (`read_texture`, as `engine.rs:376`
  does, and the frame index `SyntheticBackend` writes into the first 8 luma bytes,
  `fake.rs:226-232`): the two must hold the same frame, and the frame their tags name, counted
  apart from `unpaired_presents`. After every seek and step both lenses land within `WAIT`. The clip is longer than the cache's 1 s window ahead (`player.rs:217`), and each seed seeks forward at least once while lens 0 leads, to a time past every frame either lens holds and more than 1 s past the pair on screen, where the old pair would still be the latest common time and lens 0's landing would lie past its window from it. **Its
  control**, the same run through `open_streams` with no lock, must count both tag and content
  mismatches above 0.
- `a_locked_pair_paces_to_its_slowest_lens`, on a 90-frame clip whose clock moves a frame every
  3 ms of wall time, so a case takes under a second [C]. Cases: lens 1 at 2.5 to 3× the frame
  period; both lenses slower than the clock (1.5× and 3×); and the first case with the cache budget
  halved mid-play. **The pacing rule**, after the first 30 frames: pairs presented are at least
  80 % of the frames the slowest lens hands out; the presented time trails that lens's newest frame
  by at most 2 frames once the frame has had one present to go up; play reaches the end; the
  content check above holds; and `pair_resyncs` is 0 but in the halved case. **Its control** is the
  lock with the group base and the group `until` switched off (a test-only `LockSpec` flag), which
  freezes and must fail the rule.
- Planted faults through `PerStream`'s tables: start pts apart by more than `LockSpec.offset`,
  rates that differ, no tables, and `stss` that differ (GOPs of 8 and 10) are each refused by name
  (under P-TRK's R2 the last locks instead, and both lenses land on the later keyframe). Lens 1 a
  frame short locks, ends at the shorter lens and gives `LensShort` once. A 2×64² two-track MP4
  from M3.1's writer (7.2) with the GOP fault is refused by name through
  `FfmpegBackend::track_timing`, so the file path is tested too.
- The floor: slots killed to 3, mid-play and mid-seek, through a test hook that fails slot
  allocations as `OutOfMemory`. The pair must end unlocked within `WAIT`, never stuck. **Its
  control**, the lock without the floor, must time out.
- `FaultBackend` (`engine.rs:1494`), forwarding `track_timing` to its `inner` `FfmpegBackend`, on a 2×64² two-track MP4 without the GOP fault, its plan on lens 1 only: the pair locks (no `LockRefused`), lens 0 stays on
  hardware, pairs keep coming under the pacing rule, and the notice comes exactly once.

Tier 2h on odin: `samples_lens_pairs_frame_locked`: 100 exact seeks land both lenses on one
frame, by pts and, on the 2×3840² perf clip, by corner code; real-time play of clip 9 (handheld,
where a mixed pair shows, unlike clip 8's still tripod take) and of the perf clip with 0
unpaired. On the perf clip a region readback of the corner-code blocks reads both presented slots
on every frame, and the codes must equal each other and the tags: the content check on path B and
its pool. `bench_playback_x5_raw` locked (P-LOCK); `zero_copy_pair_matches_copy`.

## 10. Stitching in the projection pass (`render`)

### 10.1 One entry point per source kind

The stitch runs per output pixel in the pass that draws the viewer and the export (PROMPT.md:65);
no intermediate equirect is made except for a 360 export, whose output is one. `fs_project`
(equirect) stays as it is. A new `fs_rig` draws a lens pair, and `fs_eac` follows in M3.18; they
share `ray_at`, `dir_at`, `sample_plane`, `srgb_eotf`, `dim_at` and `round_f16`. The pipeline key
becomes `(Filter, DebugMode, Pass, SourceKind)` (`project.rs:450, 510-532`).

**Why not an override `SOURCE`** in `fs_project`, as the risk and verification drafts had it:
wgpu-core 30.0.1 builds an entry point's resource interface when the shader module is created
(`device/resource.rs:2459`), from naga's analysis before override values exist, and naga counts
every global a function references, under any branch; `check_stage` then refuses a pipeline whose
layout lacks one (`validation.rs:1409`, `BindingError::Missing`) [F]. So with an override every
equirect pipeline would bind lens B's group and the rig. Entry points, as `fs_flat` already is,
keep the equirect pipeline and its layout exactly as today: the 24 goldens stay byte-identical
(the refactor's gate), and a naga test asserts that `fs_project`'s interface holds no rig global.

### 10.2 Bind layout

| Group | `fs_project`, `fs_flat` | `fs_rig` |
|---|---|---|
| 0 | the view uniform | the view uniform; `Rig` at binding 1 (`layouts::view_rig`, the target's `rig_group`) |
| 1 | the slot | lens A's slot (`layouts::source`, unchanged) |
| 2 | the sampler | the sampler; the caps uniform at binding 1 and the cap texture at binding 2 (`layouts::sampler_caps`, the media's caps groups; caps off and 1×1 transparent until M3.20) |
| 3 | none | lens B's slot, in the same layout |

The rig uniform (336 B) belongs to `LinearTarget`, beside `view_uniform` and `view_group`
(`project.rs:303-304`), and `record` writes it as it writes the view uniform. A bind group fits
only a pipeline whose layout at its index matches, and `view_group` is made from `layouts::view`,
binding 0 alone (`layouts.rs:73-78`, `project.rs:353-360`), which every pipeline's group 0 is
(`project.rs:527`) and `draw` binds (677) [F]. So each target also makes `rig_uniform` and a
second group-0 bind group, `rig_group`, from a new `layouts::view_rig` (the view uniform at 0, the
rig at 1), when it is made; `draw` binds `rig_group` for `SourceRef::Rig` and `view_group`
otherwise, so `fs_project`'s layout stays as it is (10.1). Every target carries both, 336 B and one
bind group: the viewer's, the overview's, `StillRenderer`'s, the export's and Measure seam's.
In group 2 the rig would need four bind groups per rig buffer, one per sampler variant
(`project.rs:452`). **The cap texture** belongs to the media, not to a target, so it is not in
group 0, where every target's group would be remade when a cap PNG loads. It goes in group 2
beside the sampler, with the caps uniform (each pole's mode, cap, edge and rotation):
`layouts::sampler_caps` serves `fs_rig`, `fs_eac` and `fs_project_caps` alike. A media with caps
owns its four groups, one per sampler variant, made once when its PNG loads (18.3) and passed
through `SourceRef` from M3.20; `Projector` keeps four defaults (caps off, a 1×1 transparent
texture) for every other source. `fs_project` and `fs_flat` keep `layouts::sampler`. Slots keep the
bind groups they get at upload, so the DMA-BUF and path-B uploads don't change. Group 3 is the last
of wgpu's default four (`gpu.rs:462-465` keeps the defaults); nothing later needs a fifth, since M4's
stabilisation rotation goes in the view uniform and M7's LUT joins group 2. `fs_rig` uses 7 of the
16 sampled textures a stage allows.

### 10.3 The WGSL

```wgsl
struct Lens {
    rot: array<vec4<f32>, 3>, // rows of lens_from_rig; w unused
    centre: vec4<f32>,        // c_b − o in metres; w = cos θ_max
    focal: vec4<f32>,         // fx, fy, cx, cy in the slot's uv (canvas map, rect and tune folded in)
    radial: vec4<f32>,        // k1..k4
    tan: vec4<f32>,           // k5, ξ, p1, p2
    ext: vec4<f32>,           // p1', p2', s1, s2
    prism: vec4<f32>,         // s3, s4, texels per radian at θ_max, KB base (0..3, when model = 1)
    gain: vec4<f32>,          // linear RGB gain
};
struct Rig { lens: array<Lens, 2>, q: f32, feather: f32, margin: f32, model: u32 }; // model: 0 Mei, 1 KB
@group(0) @binding(1) var<uniform> rig: Rig;

fn lens_uv(l: Lens, d: vec3<f32>) -> vec3<f32> {  // uv in its slot, and cos θ
    let p = normalize(d - rig.q * l.centre.xyz);
    let v = vec3<f32>(dot(l.rot[0].xyz, p), dot(l.rot[1].xyz, p), dot(l.rot[2].xyz, p));
    let m = v.xy / (v.z + l.tan.y);               // Mei; ξ > 1 keeps the denominator ≥ ξ − 1
    let r2 = dot(m, m);
    let rad = 1.0 + r2 * (l.radial.x + r2 * (l.radial.y + r2 * (l.radial.z + r2 * (l.radial.w + r2 * l.tan.x))));
    let p1 = l.tan.z + l.ext.x * r2;
    let p2 = l.tan.w + l.ext.y * r2;
    let xd = m.x * rad + 2.0 * p1 * m.x * m.y + p2 * (r2 + 2.0 * m.x * m.x) + r2 * (l.ext.z + l.ext.w * r2);
    let yd = m.y * rad + 2.0 * p2 * m.x * m.y + p1 * (r2 + 2.0 * m.y * m.y) + r2 * (l.prism.x + l.prism.y * r2);
    return vec3<f32>(l.focal.x * xd + l.focal.z, l.focal.y * yd + l.focal.w, v.z);
}
```

`rig.model == 1u` replaces m with base(θ_d(θ))·v.xy/|v.xy| (KB, `rad` = 1), where
θ_d = θ(1 + k1θ² + … + k5θ¹⁰) and the base comes from `prism.w`: 0 equidistant θ_d, 1 equisolid
2 sin(θ_d/2), 2 stereographic 2 tan(θ_d/2), 3 orthographic sin θ_d (masked where θ_d > 90°, where
it folds), as `Lens::radius` does (`fisheye.rs:52-60`). It keeps the same tangential and prism
terms; the branch is on a uniform. `dfish_kb_rig` renders the KB rig under each of the four bases
against the CPU reference, so its control (equidistant read as equisolid) can fail through the
shader. **For each lens b:** the gradient from the four ±½ px
rays that `fs_project` already casts (`project.wgsl:181-182`), projected through the same lens,
gives λ through `lod(g, size0)`, and `sample_plane` samples with that explicit gradient
(`project.wgsl:163`). Fisheye uv has no wrap, so there is no seam spike. The mask is cos θ ≥
cos(θ_max − margin · 2^λ / texels per radian) and a **frame term**: uv in [e, 1 − e]² with e =
margin · 2^λ / the slot's side in texels, so no footprint reaches the slot's edge, where the clamp
would repeat edge texels. On Pat's V6 numbers the frame cuts the circle at 99.1° to 103.1°, by edge
and map [C] (θ_frame, 6.1), inside the reach of the strips (98° plus tilt and parallax), of a wide
feather and of a lens drawn alone. The CPU weights (6.2), `lens_coverage` (8.1) and the strips
(8.2, which mark samples outside invalid, never clamped) apply the same term, and Measure seam
draws its strips through it. `dfish_mask_margin` also draws lens 0 alone with θ_max at 104°,
past its frame edge, with the frame term off
as its control. The feather is w = 1 − smoothstep(π/2 − h, π/2 + h,
acos(cos θ)); a lens with w = 0 isn't sampled. Each lens uses its own slot's matrix and EOTF, then
its gain in linear light. The result is (w0 rgb0 + w1 rgb1)/(w0 + w1), black where both are 0,
then `dim_at` and `round_f16`. WGSL can't index textures, so lens A and lens B are two call sites
of one `sample_plane` that takes the textures as parameters. **`DEBUG_MODE = 1`** (implicit
derivatives, a negative control) needs uniform control flow, and naga 30 does not catch a
`textureSample` under a non-uniform branch through a call, so in that mode both lenses are sampled
outside the branch. New debug modes: 4 and 5 draw lens A or B alone (Measure seam's strips, the
goldens), 6 the weights (red w0, blue w1), 7 the seam difference (|lin0 − lin1| × 8 where both
cover).

### 10.4 `Projector`, `StillRenderer` and the reference

- **`Projector`:** `pub enum SourceRef<'a> { Equirect(&'a GpuFrame), Rig { lenses: [&'a GpuFrame;
  2], rig: &'a RigUniform } }`; `record` and `record_rows` take it (callers: viewer, still, blur);
  `draw` (651-685) binds group 3 for a rig. **`render::rig::pack(&Rig, [SlotGeom; 2],
  gains: [[f64; 3]; 2]) -> RigUniform`** (bytemuck, f32) is the only place the CPU's f64 rig
  becomes the shader's. The gains are the frame's (`Gains::at`, 11.1), since the rig holds none
  (6.2).
- **`StillRenderer`:** `slots: [Option<GpuFrame>; 2]` (179, 465). Its five public entry points,
  `render` (326), `render_blurred` (350), `render_yuv` (374), `render_imported` (395) and
  `render_copied` (417), all call `render_from` (435), which takes one `frame` and one private
  `Source` today (`Bytes`, `Imported` or `Buffer`, 690-697) [F]. `render_from` takes
  `SourceFrames<'a>` in their place: `One(LensFrame<'a>)` or `Pair { lenses: [LensFrame<'a>; 2],
  rig: &'a Rig, gains: [[f64; 3]; 2] }`, where `LensFrame { frame: &'a dyn VideoPlanes, from:
  PlaneSource<'a> }` and `PlaneSource` is that `Source`, made public. So each lens reaches its own
  slot by its own path, both upload before the draw, and `pack` gets both slots' `SlotGeom`.
  `render_blurred` and `render_yuv` take `SourceFrames` where they took `frame`; `render`,
  `render_imported` and `render_copied` stay one-frame wrappers that build `One`, so their callers
  don't change. `opts.flat` (the coded picture) with a `Pair` is `Invalid`; `frame --lens` and
  look's `raw` mode pass `One`. Blur records each view through `record` (`still.rs:539-558`), so
  flat and 360 outputs and blur stitch through `SourceRef::Rig`; the overview uses `record` with
  `ViewParams::equirect(IDENTITY)`.
- **Callers.** `frame`, `look`, `compare` and `sequence` reach the GPU through `agent::still`'s
  `render`, which calls `render_copied` (path B), `render_imported` (DMA-BUF) or `render_blurred`
  (the bytes), never `render` (`agent/src/still.rs:984-1000`) [F]. For a pair it calls
  `render_blurred` with a `Pair` whose lenses come the same way: two `copy_out`s under path B, two
  `import`s under `--zero-copy`, the frames' bytes otherwise; with no GPU, `reference::render_rig`.
  The export calls `render_yuv` (`export/src/pipeline.rs:1040`) with a `Pair` of its held frames,
  and M3.11b with `PlaneSource::Buffer` or `Imported`. **Test** (Tier 1, lavapipe):
  `a_pair_renders_alike_on_every_upload_path` draws one pair with each lens's planes as `Bytes`, as
  a `Buffer` and as `Imported` textures the test makes (`in_buffer` and `on_gpu`,
  `tests/upload.rs:1502, 688`), mixed between the lenses too: byte-identical. Its control, the
  lenses' buffers swapped, differs. On real decoders it is rung 7 on odin:
  `zero_copy_pair_frame_matches_the_copy`, beside `zero_copy_frame_matches_the_copy`
  (`cli/tests/tier2_frame.rs:492`), on the synthetic `.insv` under VA-API and Vulkan.
- **`reference::render_rig(lenses, rig, gains, view, size, opts) -> ImageF64`** mirrors 10.3 in f64: a
  `Source` per lens (`reference.rs:203-216`), a footprint from each lens's projected ±½ px rays
  (generalising `footprint`, 278-296, not `uv_footprint`, 59-69), the blend in `shade` (322-364)
  before rounding, and debug modes 4 to 7, so the instrument's strips have the GPU's shape. The
  naga tests (`tests/pipelines.rs:116-118`) gain `Rig`'s offsets and 10.1's interface check.

### 10.5 Goldens (D32's rules)

Each row's manifest records the calibration hash, the geometry, the gains, R and both lenses'
FNV-64, so a changed calibration reads as a changed source. The existing 24 rows must stay
byte-identical.

| Row | Check | Control (must fail) |
|---|---|---|
| `dfish_equirect_identity` (stored, 512×256) | the reference; the ideal more than 12° from the seam | equidistant applied to Mei: ≥ 6 dB worse |
| `dfish_rect_seam_left`, `_right` (stored, 256²) | the reference; the ideal in the band | the turn left out: median NCC < 0.5 |
| `dfish_zenith`, `dfish_nadir` | a twin rig (lenses swapped, view turned 180°): no pixel more than 2/255 apart | implicit LOD: over 100 bad pixels |
| `dfish_lens_only_0`, `_1` | each lens alone (DEBUG 4, 5) against the ideal over its coverage | cx + 2 px: the ideal's floor missed |
| `dfish_seam_registration` | the signature of the GPU's strips: ≤ 0.1 px rms | roll 0.5°, tilt 0.5°, f × 1.012, cx + 2, the wrong V6 reading: each attributed |
| `dfish_stitch_distance` | R = 1.5 m stitched at q = 1/1.5: ≤ 0.1 px | q = 0: a0 = 1.20° ± 0.05° |
| `dfish_exposure_match` | +0.5 EV, +5 % red, matched: step < 1 LSB | gains off: step ≥ 8 LSB |
| `dfish_feather` | lens 1 at +0.25 EV with gains Off (10.6 LSB at mid-grey [C]), on the 256² seam views over the luminance ramp: the mean of (stitched − ideal), in f64 over the pixels in each 0.45° of angle from the seam, changes by under 1 LSB between neighbouring bins (about 0.7 [C]: 10.6 LSB × 0.15/° at the smoothstep's steepest × 0.45°, a pixel's span at the view's centre). Single pixels can't serve: their 8-bit values step by whole LSBs, and near white the step itself is 18.7 LSB, 1.3 LSB a pixel [C]. The h = 0 control's step falls between two bins | h = 0: a step of at least 8 LSB |
| `dfish_hifreq_tinyplanet` (stored) | the ideal over LOD ≥ 2.5, at least 3 dB above level 0 | level-0 control |
| `dfish_lod_heatmap` | DEBUG 3 against `reference::lod_map` per lens | D32's LOD rule |
| `dfish_mask_margin` | each lens alone (DEBUG 4, 5) on the high-frequency scene, minified to λ ≥ 4 at the ring (λ recorded in the manifest): no dark ring, the mean luma over the outer 2° the mask keeps within 4 LSB of the ideal's. The stitched view cannot show it: with h = 5° a lens's weight is 0 beyond 95°, which mip bleed reaches only at λ ≥ 4 [C] | margin 0: a luma dip > 4 LSB on the ring in those views; frame term off: edge streaks > 4 LSB outside lens 0's coverage |
| `dfish_layout_invariance` | NV12, I420 and P010 within the reference floor of each other | none |
| `dfish_fullrange_left` | the X5's coding (full range, BT.709, chroma left) | tagged limited: ≥ 10 dB off |
| `dfish_kb_rig` | the KB rig through `fs_rig` (`model = 1`) | equidistant read as equisolid |
| `lrv_packed_vs_pair` (M3.14) | the packed LRV as two slots through `Columns` (`StillRenderer`'s pair) against the pair downscaled | one slot with rects: bleed > 4 LSB at the circles' edge |
| `eac_cells`, `eac_face_edges` (M3.18) | section 18.1 | one cell rotated 90°; every face mirrored |
| `nadir_patch`, `nadir_fill`, `zenith_fill` (M3.20) | section 18.3 | cap 0; the patch turned 10° or mirrored |

New stored PNGs add at most about 300 kB.

### 10.6 Cost

Outside the band one lens is sampled (today's cost plus five projections of about 15 FMAs);
inside it, both. The band at ±5° is 8.7 % of the sphere. From odin's 0.20 ms at 1080p and 0.74 ms
at 4K: about 0.5 and 1.5 ms, plus the second lens's mips [G]. Decode stays the bottleneck. P-SHADE
gates it at 0.8 and 2.0 ms.

## 11. The project: schema 2 (`core::project`)

### 11.1 Shape

```rust
pub struct Media { pub id: MediaId, pub path: PathBuf, pub fingerprint: Fingerprint,
                   pub video: MediaVideo,                  // for a camera file: the timing stream, the first lens track
                   #[serde(default)] pub source: Source,   // schema 1 has none: equirect
                   #[serde(default)] pub caps: Caps }      // nadir and zenith (M3.20)
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source { Equirect {}, DualFisheye(Box<DualFisheye>), EacPair(Box<EacPair>) }
pub struct DualFisheye { pub camera: Camera, pub tracks: [u32; 2], pub front_track: u8,
                         pub calibration: CalibSnapshot, pub geometry: Geometry, pub tune: Tune, pub gains: Gains,
                         pub profile: Option<ProfileRef> }   // {name, hash}
pub struct CalibSnapshot { pub canvas: [u32; 2], pub window: [u32; 2], pub raw: RawStrings, pub hash: String,
                           pub v3: Option<[LensCalib; 2]>, pub v6: Option<[LensCalib; 2]> }
pub struct EacPair { pub camera: Camera, pub tracks: [u32; 2], pub layout: EacLayout } // overlap, pre, post, W×H (gopro.md §2.2)
pub enum Gains { Off, Fixed { rgb: [[f64; 3]; 2] },
                 Auto { rate_hz: f64, log_ratio: Vec<[f32; 3]>, basis: String, input: AnalysisInput, estimator: u32 } }
impl Gains { pub fn at(&self, t: Time) -> [[f64; 3]; 2] }  // Off: ones; Fixed: rgb; Auto: the track at t,
                                                          // linear between samples, split as 16 says
pub struct AnalysisInput { pub file: Analysed /* Master | Lrv */, pub fingerprint: Fingerprint /* that file's */,
                           pub frames: Sampling /* Every { n } | Keyframes */ }
pub struct Caps { pub nadir: Option<Cap>, pub zenith: Option<Cap> }
pub struct Cap { pub mode: CapMode /* Fill { ring_deg } | Patch { path, fingerprint } */, pub cap_deg: f64,
                 pub edge_deg: f64, pub rotation_deg: f64 }
```

The JSON uses these names in snake_case, `"kind": "dual_fisheye"`, with the geometry's
extrinsics as `{"stored": {"axes": ["y", "x", "z"], "order": ["yaw", "pitch", "roll"], "signs":
[1, 1, 1], "roll_offset": -90}}` and q as `"q_per_m": 0.5`; `schema2.spherecut` (11.2) is the
worked example. **Hashes.** `CalibSnapshot.hash`, `Gains::Auto.basis` and `ProfileRef.hash` are 16
hex digits, as `Fingerprint.head` is (`project/mod.rs:147-153`), from core's FNV-1a 64
(`project::file::fnv1a64`, `file.rs:296`, `pub(crate)` today and made `pub` for camera, media and
the app): `hash` over the raw strings in generation order, each ended by a NUL byte; `basis` over
the little-endian bytes of both lenses' `ypr_deg` and `q_per_m`, the numbers section 16 says it
covers; `ProfileRef.hash` over the profile file's bytes [G]. testkit's `Fnv64`
(`testkit/src/lib.rs:22-48`) is a test crate's streaming hasher and is not used. **Gains** sit
beside the tune, not in it: `Gains` and `AnalysisInput` are core's types, a profile (17) carries a
tune and never a clip's exposure track, and `Rig::build` needs none (6.2).

**Self-contained.** The project holds the calibration it renders with, raw and parsed, the
geometry with its status, and the exposure track, so export, the CLI and the agents repeat a
render with no trailer reader and no cache, the file still diffs (D47), and D57's rule holds (the
server reads files, the agent never does). The parsed numbers use `sphere::rig` types, so core and
render need no camera (core already depends on sphere, D48). **No proxy field:** the LRV is a
per-viewer convenience that export and agents never use (PROMPT.md:77), found and verified by the player's open
job from its name and two trailers, never on the UI thread (section 15). **One schema for M3.** Schema 2 holds every source kind, the gains
with their Auto track and the caps, so M3 has one migration; a later change to this shape bumps
the schema to 3 with its own migration and fixture, so an older build says `NewerSchema` rather
than "unknown field".

### 11.2 Migration and `validate`

`SCHEMA` becomes 2 (`file.rs:40`). Schema 1 media have no `source`, which `#[serde(default)]` reads
as `equirect {}` (what a migration that can't probe must give); the hook at `file.rs:164` accepts
schema 1 and the value check beside `check_times` refuses a schema 2 media without `source`,
naming its path, so a damaged file never defaults silently. This keeps `from_json`'s second parse
from the text, and its line and column errors, which rewriting the `Value` would lose
(`file.rs:175`). `crates/core/tests/data/schema1.spherecut` loads unchanged; a new
`schema2.spherecut` holds a dual-fisheye source. A schema 1 `.insv` project made by an agent opens
as equirect with a notice and a one-click "Stitch it" (`set_source`).

`validate` stays free of I/O: the tracks are distinct; `generation`'s set is present; `hash`
matches the raw strings; the tune's yaw, pitch and roll within ±15°, the centre within ±64 canvas
px, the radius scale 0.9 to 1.1, θ_max 85° to 110° and below the model's limit, the feather 0° to
15° and at most min(θ_max, θ_frame) − 90° for each lens (θ_frame from the snapshot, geometry and
tune through `rig::theta_frame`, 6.1, which needs no slot size; so the band never reaches the
frame, and the pane's seam width
stops there), q 0 to 3.34 m⁻¹ (D ≥ 0.3 m), fixed gains 0.25 to 4, an Auto track with
a positive rate and finite ratios; a cap 5° to 35° with an edge of 0° to 5°.

### 11.3 Edits

`Edit::OPS` (`core/src/project/edit.rs:257`) grows from 21 to 24, each with an inverse (the old
value), a sample in `core/tests/project_edits.rs`'s per-op list (which asserts one per op, line
1095) and a reference example in `agent/src/reference.rs` (D57's test fails without one); history
labels and origins cover all three. The examples need a camera file in the reference project,
which it gains (below).

- **`set_source {media, source}`** replaces the whole source: stitching a migrated `.insv`,
  swapping the binding (status `User`), taking a measured geometry, or tagging a file as equirect
  (PROMPT.md:47). Agents' wire form is `{"kind": "equirect"}` or `{"kind": "camera", "binding":
  …?}`, which the server expands by reading the file beneath the roots, as `add_media` probes its
  path (`agent/src/ops/edit.rs:271-285`); a `set_source` whose calibration differs from the file's is refused
  with `calibration_mismatch`.
- **`set_stitch {media, tune?, gains?, profile?}`** replaces the whole tune, the whole gains (11.1)
  or both, as `set_export` does (at least one; applying a profile sends a tune only, 17); the pane's
  drags are `Gesture`s on a clone (`history.rs:222-245`) that end in one.
- **`set_caps {media, caps}`** sets nadir and zenith (M3.20).

**The reference project gains a camera file.** D57's test,
`every_op_of_the_reference_applies_and_undoes_to_what_was_there` (`agent/tests/project.rs:105-146`),
applies each example to the reference project and needs a change and an exact undo; only
`add_clip`, `remove_clip` and `remove_media` have listed refusals. The project's one media is a
3840×1920 equirect entry for an `a.mp4` that is never written (`reference.rs:204-221`,
`tests/project.rs:52-57`) [F]. On it `set_source` to equirect changes nothing, its camera form
finds no file, and `set_stitch` has no tune. So `reference::project` takes an optional second media
and adds it last in its Set up step, so that it is media 11 and every other id stays; `IDS` names
it. D57's test writes `cam.insv` with `synth::insv` (2×384², 30 frames, 7.2) into its TempDir and
makes the entry by probing it (`media::project::project_media`), so its calibration and hash are
the file's; other tests pass no second media and write no `.insv`. The three
examples name media 11: `set_source` with `{"kind": "camera", "binding": "direct"}` (the server
reads `cam.insv` beneath the roots, and the status becomes `User`), `set_stitch` with lens B yawed
0.5° and Fixed gains, and `set_caps` with a nadir fill. agent's `edit()` (`agent/src/ops/edit.rs:263-285`) expands
the camera form beside `add_media`, and `wired()` (538-550), which has no file access, skips it as
it skips `add_media`; the other two wire with no media 11 behind them.

### 11.4 Provisional geometry over time

On load and on add, each media's geometry is compared with `known::lookup`. If it is provisional and
the table's is settled or has newer evidence, the app says "This clip's lens geometry was a guess
when it was added; it has since been measured (…). Use it?", and its button applies `set_source`
labelled "Lens geometry measured". Headless, `spherecut render` adds a `warnings` entry, agents'
`project` summary shows `geometry.status`, and `check` gains `provisional_geometry` (info). Only an
edit changes a project's geometry, so an old project renders the same bytes until Pat accepts; a
Tier 1 test checks exactly that.

### 11.5 Who builds it, and tests

`media::project::project_media` (`media/src/project.rs:15-40`), which the GUI's making job
(`reframe.rs:576`) and agents call, builds `DualFisheye` with the `Known` geometry, `Tune::default()`
and the default front track when `camera::insta360` finds a calibration, and `Equirect {}`
otherwise; a raw file without a calibration keeps `not_equirect`'s doubt and shows as lenses.
**Tests:** both fixtures; a schema 2 file round-trips and schema 3 is refused; a property at 100k
that random tunes, sources and caps applied and undone through the new ops give back the same
project; `validate` at each bound and just past it; `deny_unknown_fields` still refuses a misspelt
key in `tune`; the missing-`source` refusal. **The export and viewer fixture** (rungs 4 and 5, 4.2)
is `project_media` of the rung-3 file, then one `set_stitch` with a tune that differs from the
default wherever the render can show it: q = 1/1.5 m⁻¹, lens B yawed 0.3°, a 3° feather and Fixed
gains of 0.9 on lens B. Dropping it moves lens B 1 to 5 px at the seam and its mid-grey 6 LSB at
768² [C]. Its Auto twin has the same tune with a hand-written Auto track in place of the Fixed
gains (the log ratio ramping from 0 to 0.2 over the 8 frames). The test builds the reference's
`Rig` from these numbers through `Rig::build`, never from the project, so a reader shared by the
export and the reference can't hide a tune it drops.

## 12. Export, `render` and `frame` from the pair

- **Decode.** A dual-fisheye source opens two `VideoRequest`s (`pipeline.rs:135-160`), one per
  bound track, on the path K1 chose
  (K4 on huginn): VA-API download on one thread per lens, or Vulkan Video
  download, or (M3.11b) path B or A on the export's own device (D56). `Held {n, frames:
  [Arc<VideoFrame>; 2]}` (63-66) with `DECODED_AHEAD` = 2, two pairs in the
  channel (88 MB at 8K). With each decoder's current and next frame (`decode_frames`,
  `pipeline.rs:911-983`) and the pair being rendered, 10 lens frames are alive, 221 MB at 8K [C],
  before FFmpeg's own frames and the download pools; K3 holds the total to its one-lens baseline
  and names the levers. The zip
  asserts equal `time` for each n or fails with `ExportError::LensesApart {n, times}`, checks damage
  and `concealed` per lens, and ends the export at the shorter lens.
- **Render.** `render_yuv` (1042) takes `SourceFrames::Pair`; the `Rig` is built once, and Auto
  gains are read from the stored track at each n. `placement` (758-798) stays for equirect sources;
  the forced `equirect: true` at `cli/src/frame.rs:439-440` and `agent/src/ops/sequence.rs:228-229`
  applies to equirect sources only. Audio comes from `media.path`. The export always reads the
  master. D56's own device stays, and so does its viewer-first pace, counted in pairs for a locked
  source. Today the viewer has its lead with `LEAD_FRAMES` Ready slots of any stream
  (`crates/app/src/export.rs:393-395`), which the fast lens can fill alone while the lagging one,
  sharing the video engine with the export's decoders and encoder, has nothing ahead. For a locked
  pair the lead is `LEAD_FRAMES` clip times after the shown one at which every member has a Ready
  slot (`pairs_ahead`, a pure sibling of section 9's `pick_pair`, added in M3.11), or as many as
  each member's share of the slots holds beside its shown frame and one a draw holds: one at 6
  slots, two at 8. `stand_aside` and `yield_to_viewer` cover both decoders.
- **Tests.** Tier 1 (rung 4): flat (on a side seam) and 360 exports of 11.5's fixture, the rung-3
  file with a tune that isn't the default, against `render_still` of a `Rig` the test builds from
  the same numbers, never read from the project; libx264 and libx265 at the high preset, at M2's
  floors. **Controls** through an `ExportFault` hook must each miss: lens 1 a frame late; both a
  frame late; the tune dropped (the export builds its `Rig` with `Tune::default()`); and, on the
  fixture's Auto twin, gains off under Auto. The app's
  `the_export_yields_while_a_locked_pairs_lens_lags`: `SyntheticBackend` holds lens 1 back while
  lens 0 has two Ready slots ahead (at 6 slots each member's quota is 3, `upload.rs:360`, and its wanted list starts at the shown frame at Tg, `upload.rs:419`, so two is the most it holds ahead), and the export's stages yield (`yielded` > 0); its control
  applies today's per-slot count to the same slots and must fail, since it finds a lead. Tier 2h
  on odin: `stitched_export_matches_preview` (clip 1, VA-API, flat 4K on a side seam and 360) at
  M2's VA-API floors, 47.7 dB in H.264 and 50.1 in HEVC (odin's 50.8 and 53.2 on the synthetic
  grid, less 3 dB; `crates/export/tests/tier2_export.rs:223-244`). Clip 1's own numbers are
  recorded; if they sit under those floors, the floors become clip 1's less 3 dB in a reviewed
  change. Its controls are the two frame-late ones (consecutive frames of clip 1's lens 0 differ
  by 31 to 40 dB even at 960² [F]) and a planted gain fault in place of gains off: the export
  renders with Fixed gains of 1.25 on lens A and 0.8 on lens B, which move mid-grey 14 and 13 LSB,
  about 25 dB, on either lens [C]. Gains off can't serve on real footage: at a 1 % ratio between
  the lenses, P-GAIN's "matching off" case, it moves mid-grey about 0.3 LSB and leaves the export
  near 49 dB in H.264 and 50 to 52 in HEVC, above both floors [C]. `bench_export_4k30_from_insv` at
  ≥ 30 fps, frames over the whole export as M2's gate counts them (`tier2_export.rs:448`), from
  5.7K30 (all 600 frames of the synthetic 2×2880², then take 16's 20 s) and from 8K30 (all 966
  frames of clip 8 [F]; clip 1's 114 would read a 36 fps export with half a second of start-up as
  31 fps [C]), with each stage's time and the RSS high-water mark against D42's huge-page slump.
  On huginn both rows read "pending huginn" until its answer. A miss after K1's ladder (K4's on
  huginn) goes to Pat then, as a decision card with each stage's time and two or three options:
  gate 5.7K30 only and record 8K30; accept an 8K30 export below real time, at its measured rate;
  or open M3.11b if it was closed. The 8K30 row is not narrowed away silently.

## 13. The app

### 13.1 The bug first

An agent-made `.insv` project opened in the GUI is dropped [F]: `poll_loading` opens one stream
(`reframe.rs:723`), the probe finds two lenses, and `open_lenses` calls `reframe.let_go()`, which
sets the doc to `None` (`app.rs:605-617`). M3.0 keeps the doc and shows the raw view inside the
project; the kittest `an_agent_made_insv_project_stays_open` fails on main first. M3.10 stitches it.

### 13.2 The viewer and the overview

`DrawKey` (`viewer.rs:35-40`) becomes `{slots: [usize; 2], frames: [SlotFrame; 2], view, filter,
debug, rig_rev}`, so a tune change redraws, and `app.rs:1204-1233` projects both lenses through
`SourceRef::Rig`. `Doc.rig: Option<(Rig, u64)>` is rebuilt when the source's revision changes
(microseconds). `raw_view()` (`app.rs:645-656`) is false for a locked pair with a calibration; the
raw view stays for refused locks, files without calibration, a `Known` entry with
`stitchable: false` (5.3), and View ▸ Raw lenses, which draws
each lens's 90° and θ_max circles. The overview (`viewer.rs:514-515, 579`) draws the stitched
equirect with the band shaded (DEBUG 6 at low alpha) and both lens axes marked. The overlay gains
`unpaired`, decode ms per lens and the projection time. Until M3.11 the export window says
"Stitched export arrives with M3.11" for a dual-fisheye media.

### 13.3 The Stitch pane

`Pane::Stitch` joins `SIDE` (`app.rs:37-60`); old saved layouts gain it (`app.rs:103-106`).

- **Geometry**, read-only: its status badge and evidence, **Swap lenses** (`set_source` with the
  binding flipped, status `User`), and **Use measured geometry** when 11.4 applies. The conventions
  (binding, canvas map, Euler class, tz, upright, generation, reading) sit behind **Advanced** and
  the CLI's `--geometry`: they are engineering switches that take 14 settles, not choices for the eye.
- **Lens A and lens B** (front and back once take 15 says which): yaw, pitch, roll (°), centre x
  and y (px), radius (%) and FOV (°), each an Inspector `number()`, one undo step on release.
- **Seam:** width (°), distance in metres with ∞ (stored as q, the slider linear in q), the seam
  view (off, weights, difference), **Measure seam** and **Estimate distance**.
- **Exposure:** Off, Fixed (with **Match now**) or Auto, with the measured ratio and **Analyse**
  when the track is stale (16). **Profile:** Apply ▾, Save as…, Use for this camera (17).

### 13.4 The seam drag, Measure seam and Estimate distance

**Alt-drag** in the viewer near the seam turns lens B with `sphere::grab::solve_free`, so the point
lens B shows under the cursor stays there; Shift+Alt with a vertical drag changes q; each is a
`Gesture` ending in one `set_stitch`. **Measure seam** is a `Job`: it draws the two strips at the
current frame (DEBUG 4 and 5) into a 1440×128 target, reads them back on a worker through
`wait_for_submitted` (D56, so the UI never waits), runs `render::seam`, and shows, for example,
"0.8 px rms; roll −0.12°, scale +0.03 %, tilt 0.05°; photometric step 0.4 EV", naming the dominant
term with a hint ("roll: drag lens B along the seam"). **Estimate distance** runs the per-sector q
fit over the current time ±1 s on the master's frames decoded in software off the UI thread, and
ends in one `set_stitch` labelled with its name. It decodes 2 to 4 s of each lens, from the keyframe
before t − 1 s, so 5 to 10 s in software at 8K30 on odin [C]. Like the export (D56, Pat's choice),
it stands aside while the viewer plays: `Pace::stand_aside` (`export/src/pace.rs:131`, the pace
the UI sets at each pass, `app.rs:402-404`) runs before it opens its decoders and before each
frame, so it starts nothing while the viewer plays, the frame in flight finishes, and Cancel still
ends it at once. The pane says "Waiting while the viewer plays". Its kittest,
`estimate_distance_stands_aside_while_playing`, presses it, plays 20 steps and finds no frame
decoded until play stops. Measure seam draws from the slots on screen and
decodes nothing, so it never waits.

### 13.5 Notices, jobs and tests

Notices: a lock refused and why; no calibration; geometry provisional (once per media, in Info);
geometry measured (11.4); "Stitch it" for a pre-M3 `.insv` project; a proxy missing. Profile I/O,
the exposure analysis, Measure seam and Estimate distance are `Job`s, and the analysis and
Estimate distance stand aside while the viewer plays (13.4, 16). The clippy.toml ban on blocking
calls holds in app code; camera's reads call `may_block` and run only on the player's `OpenJob`
(the proxy toggle's reopen included), the making job and the profile jobs (5.1), so the runtime
guard covers them.
**Tests** (kittest, lavapipe): opening the synthetic `.insv` stitches, and the viewer's readback
equals `StillRenderer` within 1 LSB (rung 5); a yaw edit changes only its lens, in one undo step,
and bumps `rig_rev`; Swap lenses and each seam view; the bug test (13.1); `never_blocks` with the
pane open, a slow profile directory (M3.16) and a slow exposure job (M3.15), and (M3.14) with an `.insv` beside its LRV
opened and the proxy toggled on and off, the backend gated and the harness thread marked, so a
camera read on the UI thread panics through `may_block` or holds a step past 250 ms (control:
`read_trailer` called on the marked thread panics); `analysis_stands_aside_while_playing`
(M3.15): with an Auto analysis pending, 20 played steps decode no analysis frame, the analysis
then ends in one "Exposure match" step, and Cancel while it waits ends it at once; 13.4's tests in
M3.13. Tier 2h:
`gui_smoke` on clip 8 at 8K30 stitched, ≥ 99 % presented, UI p99 within refresh, with seam
screenshots for Pat's eye-check list; and, from M3.15, the same run with an Auto analysis pending
(clip 8 opened with its track stale) at the same gates, dropping no more frames than the run
without it.

## 14. CLI and agents

| Surface | Change | Slice |
|---|---|---|
| `spherecut probe`, agent `probe` | the camera block (5.4); folder listings group the master with its LRV through `companions` | M3.2 |
| `spherecut doctor --file` | stitchable or not, and why; both lenses on the chosen decode path | M3.2 |
| `spherecut frame <insv>` | stitches by default with the file's calibration and the `Known` geometry, through `agent::still::Still::open_pair` (two decoders, an exact seek of both to one time, equal pts asserted), then `render_blurred` with a `Pair` on the run's upload path (10.4); `--reference` for the CPU reference; `--projection equirect`; `--lens 0\|1` for one lens raw; `--seam weights\|difference`; `--geometry binding=…,canvas=…` and `--stitch-distance 2m` for a file without a project | M3.5 (CPU), M3.7 (GPU) |
| frame's hint (`frame.rs:896-904`), check's `no_sphere` (`check.rs:872-881`), `equirect_doubt` (`agent/src/still.rs:181`, through `check_equirect`, 236-316), look's `assume_equirect` (`look.rs:229, 1078`) and its hint (693-696) | a camera file with a calibration is not doubted and points to `project create` and `render`; one without says "no calibration: shown as its lenses" | M3.7, M3.9 |
| agent `look` | an `.insv` with no project stitches (M3.7); `mode: "seam"` tiles both side seams and both poles with lens A, lens B and their difference × 4, and returns the signature in the JSON (misregistration px, terms, NCC, photometric step, the dominant term) | M3.7, M3.13 |
| agent `project create` (`ops/project.rs:160-201`) | builds `dual_fisheye`; warns `no_calibration` and falls back to a generic KB equidistant 200° rig with centred circles, labelled so, only when the trailer has none; the summary (486-505) shows the source kind, camera, geometry status, tune and profile | M3.9 |
| agent `edit` | `set_source` (filled by the server), `set_stitch`, `set_caps`; `apply_profile {media, path}` reads a profile beneath a read root (D45) and records `set_stitch` with the profile as provenance | M3.9, M3.16, M3.20 |
| `look`, `compare`, `check` with a project, `frame --project`, `render` (CLI and agent) | render the pair; before M3.11 a dual-fisheye project's renders are refused with `not_yet` ("stitched rendering of projects arrives in M3.11"), never stubbed | M3.9, M3.11 |
| `docs/agents.md`, SKILL.md, instructions | a recipe: probe an `.insv`, create, `look` at the seam, nudge with `set_stitch`, render | M3.13 |

Tier 1: probe's JSON contract; `frame` on the synthetic `.insv` (GPU within D32's CPU rule, and
`--reference`); `project create` gives the trailer's hash; `set_source` refused on an altered
calibration; `look seam` on a planted 0.5° roll reports roll 0.5° ± 0.05° and names it;
`provisional_geometry`; the new ops' reference examples; `not_yet` before M3.11 and gone after.

## 15. LRV proxies

`companions` names the `LRV_…_01_NNN.lrv` that belongs beside the master; the player's `OpenJob`
looks for it, opens both files and has `verify_companion` check record 4, so neither file is read
on the UI thread, the proxy toggle's reopen included (5.1). The
proxy is a packed source over one stream whose two halves must show all or none, as a lens pair's
do. Slots, uploads and `pick` go by stream and time (`slots.rs:93-104`, `playback/src/upload.rs:377-391`,
`player.rs:1344-1360`), and showing two slots of one frame together is the lock's own present
problem. So **the proxy opens as a lock group of two members over the LRV's one stream**:
`open_locked(lrv, [0, 0], LockSpec { columns: Some([(832, 832), (0, 832)]), .. })`, each member's
first column and width. Each member decodes the stream with a decoder of its own (two entries for
one stream are two player streams, which `open_streams` already allows, `player.rs:713`,
`worker.rs:190`) and keeps its half through `Columns` (below). Member b shows track b's lens, so
track 0's member takes x = 832 and track 1's x = 0, from `lrv_half_track = [1, 0]` [F]. Section 9's
`pick_pair`, group seeks, catch-up, end, slots and fallback apply unchanged, and the open checks
agree trivially. The master's calibration and tune apply with s = 832. The cost is the second
decode: at worst half of perf.md's one-decoder rates, 15 to 17× real time on odin's Vulkan Video
and 26 to 30× on huginn's VA-API [C], with the packets (8.0 to 8.7 Mb/s [F]) read twice. Each half
uploads as its own slot, so no mip mixes the lenses (D31's bleed) and the shader can't tell it is a
proxy. D31's crop keeps only the top-left part and nothing takes an origin (section 2), so the crop is a
view: `render::upload::Columns { frame, x0, width }` implements `VideoPlanes` over a frame's columns
x0 to x0 + width. Its size is (width, height). Each plane starts at its first column (x0 shifted by
the plane's subsampling, times its bytes per sample position: 832 bytes into the luma and into
NV12's chroma for the right half, 416 into each I420 chroma plane [C]), with the frame's stride and
its own width; `dmabuf` and `vulkan` are `None`. x0 and width must be even under 4:2:0, which keeps
the chroma siting (left) as it is. `upload_placed` takes a `Columns` with `Placement::FULL` as it
stands (its checks and `write_plane` read rows `stride` apart from the plane's first byte), and so
do `StillRenderer` and the CPU reference, so rung 6, `lrv_packed_vs_pair` and the viewer share one
crop. In playback the upload pass takes each member's columns from the `LockSpec` beside its
placement (`playback/src/upload.rs:279-295`), and `upload` (465-557) wraps the frame in them for
the slot's shape and for each path below. **Paths, as D25 has them.** Proxies take the app's
default path. On odin `Auto` picks Vulkan Video for H.264 on the discrete GPU and the viewer's
device is shared (`decoder.rs:303-309`, `worker.rs:281-296`), so path B: the Copier copies the
member's whole picture as today (`copy_out`, 633-661, with `shown_regions` of `Placement::FULL`,
unchanged), and `upload_from_buffer` fills the slot from `Columns::buffer_planes(&copied.planes)`,
each plane's offset moved to the member's first column. One check changes: `upload_from_buffer`'s
end (`upload.rs:355`), offset + bytes per row × height, becomes wgpu's own rule, offset + bytes per
row × (height − 1) + one row, since the right half's chroma ends 832 bytes past the Copier's buffer
under today's check and 128 bytes inside it under wgpu's [C]. Elsewhere (huginn's VA-API, software,
a device that can't be shared) proxies take the copy path: `upload_placed` of the `Columns`,
directly or after the download fallback (588-599). Path A stays opt-in (D25); with it on, a member
with columns asks VA-API for downloaded frames (`on_device`, `worker.rs:276-306`), since
`upload_imported` copies from the texture's corner. So proxies need no exception to D25. **Time:** at master clip time
t the proxy shows the LRV frame with the greatest pts ≤ t (never by index: at 59.94 and 119.88 the
LRV holds a half and a quarter of the frames), plus P-TRK's LRV offset if any. Settings
(`settings.rs:69-87`) gain `proxies: Off | WhenPresent`, a per-viewer toggle that reopens the
player at the same time; the overlay says "proxy"; the default is question 3's. Export and agents
always read the master.

**Tests.** Tier 1: the LRV frame-count rule as a property over rates and lengths;
a property at 100k that `Columns`' planes equal a cut-out copy's, row for row, for NV12, I420 and
P010 at random even x0 and width, an odd one under 4:2:0 refused; on lavapipe, a `Columns` upload
equals the cut-out's byte for byte through `write_texture` and the belt (control: x0 off by 2
differs); `shared_vulkan.rs`'s one test (lavapipe's test-only shared device, D25) gains a packed
case, both halves' slots filled from one Copier copy through `Columns::buffer_planes` equal to the
copy path's byte for byte (control: the planes left unshifted differ);
`a_packed_proxy_never_shows_its_halves_apart`, beside section 9's slow-lens test: a `SyntheticBackend`
clip opened as the proxy's group, member 1's decoder 2.5 to 3× slower with jitter (the slow lens set
per decoder, not per file stream), 64 seeds of random seeks, plays, steps, pauses and rates, 0
unpaired, and its control, the same run with the lock off (each member picked on its own), must
count more than 0; `lrv_packed_vs_pair`; rung 6 at its blessed floor, through the app's viewer
with proxies on (the slot ring and `pick_pair`), with the halves swapped as the control (3.4's swapped-tracks row: a tilt of
twice the lenses' angle difference and the centres' difference along the seam; NCC does not
collapse, 8.2); an export with proxies on decodes the master. Tier 2c: `lrv_matches_master` on every
sample pair (the SSIM against the stitched master downscaled at matched times, over 20 frames a pair spread
across its 30-frame GOPs [F], keyframes and mid-GOP frames both; controls: the halves swapped, a one-frame offset on
clip 9). Floors before gates (4.1 rule 3): M3.14 records each pair's median SSIM and its σ, the
standard deviation of the per-frame SSIM; a reviewed change then gates the row at the lowest median
less 3 times the largest σ, and each control must fall below that gate. No SSIM threshold is fixed
before the floor is measured. Tier 2h: stitched LRV seeks under 150 ms at p95
(21 to 22 ms unstitched today [F]); `bench_playback_x5_stitched` on clip 8's LRV at 0 dropped, on the app's default path (path B on
odin, D42); `zero_copy_lrv_matches_copy`, beside `shared_vulkan_matches_copy_path`
(`playback/tests/tier2_playback.rs:1915`): every sample LRV's two halves byte for byte through path
B and through the download.

## 16. Exposure and white-balance matching

The trailer has no per-lens exposure or white balance [F], so the match comes from the overlap.

- **The estimator** (`render::seam::gain`): per frame, sample the band at the stitch's own
  parallax-corrected rays on coarse mips (about 64 × 512 samples per lens); take the per-channel
  median of L = log(L1/L0) in linear light, leaving out samples above 0.95 or below 0.01 and the
  20 % with the highest gradient, holding the last value when fewer than 200 remain; split it
  symmetrically, **g0 = exp(+L/2), g1 = exp(−L/2)**, so both meet at √(L0·L1) and the level stays.
- **One analysis per clip**, a background job, on the LRV when verified (cheap and exactly scaled),
  else on the master, at 4 Hz. **The smoothing** has zero phase: a one-pole filter with time
  constant τ = 1.2 s (a = exp(−1/(4 Hz × τ)) per sample) runs forward, then backward, each pass
  starting from its segment's first sample, so a constant passes through unchanged. A segment
  restarts at each of record 4's exposure steps, the only use M3 makes of record 4.
- **On the master it is a full decode** of both lenses. At 8K30 and 8K25 every frame after an IDR
  is a reference frame (TRAIL_R, on clips 1, 2 and 8) [F], so taking every 6th frame saves no
  decode, and FFmpeg's HEVC decoder has no reduced-size output; only the estimator works at
  reduced size, on coarse mips. At 59.94 and 119.88 every other frame is a non-reference TSA_N
  (clips 3 and 4) [F], which the decoder skips (`AVDISCARD_NONREF`), taking the decoded frame
  nearest each 4 Hz instant. It decodes on the export's path (K1's choice, `HwOutput::Download`)
  where there is hardware, else in software: clip 9 (63.5 s at 8K30) takes about 33 s on odin's
  Vulkan Video (1.93× real time for the pair) and about 151 s in software (0.42×), from
  perf.md's pair rates [C]. On a machine with software decode only, keyframes alone (0.5 Hz at 8K30,
  recorded as `input: keyframes`) are offered if M3.15's P-GAIN report shows that such a track
  stays within 0.5 % of the 4 Hz one on every take.
- **It stands aside while the viewer plays**, as the export does (D56, Pat's choice):
  `Pace::stand_aside` (`export/src/pace.rs:131`, the pace the UI sets at each pass,
  `app.rs:402-404`) runs before it opens its decoders and before each frame, so it starts nothing
  while the viewer plays, the frame in flight finishes, and Cancel still ends it at once. The pane
  says "Waiting while the viewer plays". Measure seam decodes nothing and never waits.
- **Stored in the project** (`gains.auto`: rate, log ratios, `basis`, input, estimator version), so
  the viewer, the export, `frame` and agents read the same numbers at each frame. A causal filter
  in the viewer and a zero-phase one in the export would break "export matches preview"
  (PROMPT.md:61). `basis` hashes the tune's rotations and q, so a `set_stitch` that changes them
  marks the track stale; the app reruns the analysis once the pane has been quiet for 1 s with the viewer stopped and
  applies it as one edit, "Exposure match", using the old track until then. An agent's
  `set_stitch` with `{"mode": "auto"}` and no track applies with the gains as they were and starts the analysis as a D58 job, whose id
  the result gives: `job` reports on it, waits for it or cancels it, and it ends in one edit,
  "Exposure match", that stores the track, or fails `stale_basis` if the tune's basis changed
  meanwhile. `edit` itself never decodes.
- Gains apply in linear light, after each lens's EOTF and before the blend; on SDR footage this is
  approximate in the highlights, where the camera's tone curve bends. The default follows P-GAIN,
  and M3.15's note gives Pat its numbers.

**Tests.** Tier 1 properties at 100k: a constant planted gain recovered within 1 % with noise and
5 % outliers; clipped samples don't bias it; a ramp (lens 1 from 1.0 to 2.0 over 3 s, in a 12 s clip)
matches the truth's log ratio run through the same filter (the test's own forward-backward
one-pole) within the constant case's 1 %, so it measures the estimator, not the filter, which
rounds the ramp's corners by up to 15 % [C]; its control, a causal one-pole, misses by 16 % [C]. `dfish_exposure_match`. Parity: an export with Auto equals the preview (**control:** a
viewer with a causal filter fails it). A stale-basis test; 13.5's `analysis_stands_aside_while_playing`. Tier 2c `exposure_step_after_match`:
the step across the seam before and after, recorded on clip 9 and on take 17, each p95 with its σ over disjoint 1 s windows; a reviewed change then gates take 17 at < 2 LSB at p95 only if its p95 lies at least 3σ under that, and otherwise at its p95 plus 3σ, with the slice note saying which (4.1 rule 3).

## 17. Stitch profiles

`core::dirs` gains `config_home()` and `data_home()` (`$XDG_CONFIG_HOME`, else `~/.config`), and
`cli/src/skill.rs:18-25`, which works the data dir out by hand, moves onto them. `camera::profile`
stores `$XDG_CONFIG_HOME/spherecut/stitch/<vendor>-<model>-<serial>/<name>.json` as `{format:
"spherecut.stitch-profile", schema: 1, name, camera, calibration_hash, geometry?, tune, saved}`
through `TempFile::persist_replace` (D16); its list, load and save call `core::blocking::may_block`
first (5.1), and the app runs them only as jobs (13.5); `geometry` is present only for a fitted or user geometry
(8.7). Applying a profile is a `set_stitch` (and a `set_source` if it has a geometry) with `profile
{name, hash}` as provenance, so a later edit of the profile never changes an old project. A profile
for another calibration hash (a factory recalibration) applies with a warning naming both. "Use
for this camera" marks the serial's default, which a new project applies with a notice and an
undo. eframe's storage (`run.rs:26, 166`) is not used: the CLI and agents can't read it. Agents
reach profiles only beneath a read root (D45). **Tests:** save then load is identity; another
serial's profile isn't offered; editing a profile leaves a project unchanged; a project whose
profile was deleted renders the same; a corrupt profile is listed with its error, never applied;
an atomic write under a full-disk fake leaves no file.

## 18. Later in M3

### 18.1 The GoPro MAX's EAC pair (`fs_eac`)

Built on `docs/formats/gopro.md` (2026-10-22, with real MAX pixels measured from max2sphere's test
frames). Two HEVC tracks of 4096×1344 (5.6K) or 2272×736 (3K) on the MAX, 5952×1920 or 5888×1920
on the MAX 2; track 0 holds left, front and right upright, track 1 down, back and up, each turned a
quarter; the side cells hold two registrations overlapping by 2·O (§2.1 to 2.5). `camera::gopro`
reads `PRJT` and `PMOD` from the `moov/udta/GPMF` header blob with a minimal KLV walk of that blob
only (gopro.md derives the layout from `PMOD`, §2.2; its MAX 3K row doesn't fit the table), and
falls back to the (W, H) table with a warning. The `gpmd` track stays M4's. `Source::EacPair` locks
its tracks like the X5's. `fs_eac` picks the face with the largest dot product
(§2.3). gopro.md's frame (right, up and forward along +X, +Y and +Z) is left-handed, so camera
converts its table to D33 by negating z and keeping x and y, a reflection (a rotation, such as
180° about Y, would mirror every face), and a unit test asserts det[r d f] = +1 for every face
afterwards. Sampling is
equi-angular in s and t, reads a centre cell directly and blends a side cell's two registrations
linearly across the 2·O band (§2.5's clamps); LOD is per face from the projected rays, and samples
stay 2^λ texels inside their cell; a CPU reference twin. **Tests:** `synth::eac`'s own writer,
which builds each cell's directions from D33 and the YouTube EAC layout as FFmpeg's `v360` defines
it (the layout gopro.md §2.3 and §2.8 find GoPro's cells match once the overlap is removed), never
from gopro.md's table or camera's conversion of it,
and a two-stream synthetic `.360`; rungs 1 to 4; `eac_cells` and
`eac_face_edges` (a face edge against a roll twin, ≤ 2/255; control: one cell rotated 90°, a step
over 8 LSB); `eac_cells` also carries an asymmetric glyph on FRONT and on UP that must match the
ideal unmirrored, with camera's conversion done as a 180° turn about Y (every face mirrored) as its
control; with samples (Tier 2c), max2sphere (Apache-2.0) built out of tree renders reference
equirects that ours must meet at a blessed floor, and the lock tests run as for the X5. GoPro's
LRV has no known calibration, so it is no proxy in M3. Without samples: "done pending samples".

### 18.2 DJI Osmo 360

dji.md §10 already finds raw feasible without an SDK, so the slice starts there. With samples,
`camera::dji` detects the format by its `djmd` track and `product_name`, reads the first `djmd`
sample with camera's varint walker (no `prost`), and maps the calibration block
(§3.2, with the field numbers of OpenOSV's `proto/dvtm_osmo360.proto`, section 20) to a `Kb` lens
on the equidistant base with k1..k5 and tangential terms
(§3.3, `model = 1`). The conventions are data again (the
Euler composition [U], the 6K window [S], which slot): `camera::dji::known` starts provisional on
`native_refine` (slots 1 and 2, as OpenOSV does; DJI's default `far_11` is a stitcher constant,
4.1 rule 7),
and the same fit settles it on an open-field take. Without samples, dji.md records the M3 state
and the stitched-export path is tested: probe tags DJI's export with its camera and projection,
and `set_source {"kind": "equirect"}` tags it in a project (agents' `edit`, M3.9; the app's
"Treat as equirect", M3.10). No DJI stitcher constants (dji.md §11.2).

### 18.3 Nadir and zenith caps

`Media.caps.{nadir, zenith}`: fill (from the ring) or patch (a PNG through `image`, RGBA8 sRGB,
loaded as a job), cap 5° to 35°, edge 0° to 5°, rotation, anchored to the rig (a world anchor needs
M4's gravity). The step runs after the source, in rig space, before the view, so a patch follows
the tripod and M4's stabilisation turns it with the picture. A patch PNG is seen from inside,
looking at its pole with the head tilted from facing forward: at the nadir, forward (−Z) at the
PNG's top and +X at its right; at the zenith, forward at its bottom and +X at its right (sphere's
`Down` and `Up` faces, `cube.rs:46-49`). With α the angle from the pole and φ measured in the PNG
from its right towards its bottom (atan2(z, x) at the nadir, atan2(−z, x) at the zenith), a patch
inside α < α_cap samples at uv = ½ + ½(α/α_cap)(cos(φ − rot), sin(φ − rot)), so `rotation_deg` > 0
turns it clockwise as seen; premultiplied, with a smoothstep edge. The radial mapping is
nadir-patcher's polar equidistant one; the orientation is ours, since nadir-patcher
flips its logo both ways. Fill samples
the source at (φ, α_cap + ring) at a coarse LOD. For `fs_rig`, `fs_eac` and `fs_project_caps` the patch
and the caps' numbers are in group 2 beside the sampler (`layouts::sampler_caps`, 10.2), in four
bind groups that the media's caps own, made when the PNG
loads; an equirect source with caps uses the twin `fs_project_caps`, so `fs_project`'s layout
and goldens never change. **Tests:** synth plants a dark tripod disc; `nadir_fill` (luma variance
inside the cap under a bound, no edge step over 2 LSB; control: cap 0), `nadir_patch` (the polar
mapping against the CPU reference, and an asymmetric glyph at both poles against synth's own
rendering of it, written from this paragraph's words, in 90° views looking straight down and up;
controls: the patch turned 10°, and the patch mirrored), `zenith_fill`; parity.

## 19. Tests

Each section lists its own tests; this section gathers them by kind.

### 19.1 The Tier 1 round trip

The ladder of 4.2 with its scene, rig, thresholds and controls: rung 0 (M3.3), 1 (M3.4), 3
(M3.5), 2 (M3.7), 5 (M3.10), 4 (M3.11) and 6 (M3.14). The attribution matrix (8.3), the
planted-truth recovery (8.5), and Measure seam's and `look seam`'s attribution tests ride on the
same scene and rig.

### 19.2 Golden rows

The `dfish_*` rows of 10.5 (M3.7), `lrv_packed_vs_pair` (M3.14), `eac_cells` and `eac_face_edges`
(M3.18), and the caps' three rows (M3.20), each with its control, on lavapipe and on RADV under
D32's other-driver rule. The 24 existing rows stay byte-identical.

### 19.3 Property tests (100,000 cases each)

Unified, the canonical readings, rig, canvas maps, weights and distance (6.3); `EulerConv`
distinctness and its twins (`euler_twins_are_quarter_turns`, 6.2); strings and trailer round trips, truncation and arbitrary bytes (5.5); `pick_pair`
(9); random edits and undo with the new ops, and schema 2 save then load (11.5); the gain
estimator (16); the LRV frame map and `Columns` (15).

### 19.4 Tier 2 gates

| Test | Gate | Tier, where | Slice |
|---|---|---|---|
| `samples_x5_tracks_equal` | P-TRK's verdict per field | 2c, cloud | M3.0 |
| `bench_export_pair_decode`; `bench_playback_x5_raw` on the two-track perf clips | K1 and K3 on odin; K2 and K4 on huginn, recorded when its answer comes | 2h, odin and huginn | M3.1 |
| `samples_insv_trailer_parses`, `samples_calib_print_is_identity` | the 14 files as in 5.5 | 2c, cloud | M3.2 |
| `rig_fit_clip8_10_provisional`, `samples_thumbnail_orientation` (if Pat agrees), `samples_lens_offset_is_zero` | recorded with σ (8.4) and the directions stage F leaves undetermined; k = 0 by 5σ, with 4.3's σ | 2c, cloud | M3.6 |
| goldens on RADV; P-SHADE | D32; ≤ 0.8 and 2.0 ms | 2h, odin | M3.7 |
| `samples_lens_pairs_frame_locked`; `bench_playback_x5_raw` locked | 0 unpaired by tag, and by corner code on every frame of the perf clip; P-LOCK, 8K25 and the pacing rule included | 2h, odin | M3.8 |
| `project_create_x5` | a schema 2 source with the trailer's hash | 2c, cloud | M3.9 |
| `gui_smoke` stitched; `bench_playback_x5_stitched` | ≥ 99 % presented, UI p99 within refresh; 1080p and 4K on the synthetic 2×2880² `.insv` at 29.97 (7.2): 0 dropped at ≥ 1.2× unthrottled; 8K30 and 5.7K60 recorded | 2h, odin | M3.10 |
| `stitched_export_matches_preview`; `bench_export_4k30_from_insv` | M2's VA-API floors (47.7 / 50.1 dB) with the frame-late and planted-gain controls; ≥ 30 fps over the whole export from 5.7K30 (600 frames) and 8K30 (all of clip 8); huginn's rows pending until it answers | 2h, odin, huginn | M3.11 |
| `rig_fit_take14`, `samples_binding_take15`, `seam_quality_take14`, `seam_quality_clip8` | 8.6's rules; seam rows gated at the floor + 3σ, σ as 8.4 defines it (over GOPs, at least 0.05 px), with the spread between clips 8, 10 and 14 beside it | 2c, cloud | M3.12 |
| `lrv_matches_master`; stitched LRV seek; `zero_copy_lrv_matches_copy` | SSIM recorded with σ, then gated at the floor − 3σ, with controls; p95 < 150 ms | 2c; 2h | M3.14 |
| `exposure_step_after_match` | recorded on clip 9; < 2 LSB at p95 on take 17 | 2c | M3.15 |
| the full hw-test, take 16's 5.7K30 rows | every row passes or is reported with its number and stage; huginn's may read "pending huginn" | 2h, odin, huginn | M3.17 |

### 19.5 The performance targets (PROMPT.md:129-136)

| Target | Where it stands | M3's gate |
|---|---|---|
| The UI at display refresh | `gui_smoke` | `gui_smoke` stitched (M3.10, M3.17) |
| 5.7K30 raw in real time on odin and huginn | odin: 143 fps per lens at 5.7K60 in the raw view (path B); huginn: VA-API pair decode 1.61 to 1.95× at 5.7K60, no engine run | `bench_playback_x5_stitched` on the synthetic 2×2880² and take 16; huginn's K2, when its answer comes |
| 8K30 in real time, at least on proxies | the LRV decodes at 24 to 35×; odin raw 8K30 path B 81 fps per lens | the stitched LRV at 0 dropped (gated); raw 8K30 recorded |
| A seek under ~150 ms on proxies | LRV exact seek p95 21 to 22 ms; raw 8K p95 370 to 380 ms | the stitched LRV pair seek, p95 < 150 ms |
| A reframed 4K30 export at real time | 32.5 fps from 5.7K equirect, decode-bound; raw 8K has 1.8× the pixels; huginn has never run an export | `bench_export_4k30_from_insv` ≥ 30 fps over the whole export from 5.7K30 (600 frames) and from 8K30 (all of clip 8) on odin, and on huginn when its answer comes, after K1's and K4's ladders; a miss goes to Pat as a decision card with the stage times |

The stitch shader's cost is P-SHADE's (≤ 0.8 ms at 1080p, ≤ 2.0 ms at 4K on odin).

### 19.6 Budgets for Tier 1

Each new suite prints its time against its budget: camera ≤ 1 s, its 100k run apart (M3.2);
sphere's properties and rung 0 ≤ 30 s in release (M3.3); metrics, seam self-tests, the
attribution matrix and rung 1 ≤ 15 s (M3.4: about 5 pair renders at about 2 s each, 7.1, the ideals, `render_rig` for about 14 controls and their strips [G]); rung 3 and `frame --reference` ≤ 3 s (M3.5); the fit's
recovery ≤ 15 s (M3.6: 2 pair renders, 20 encoded frames, stage G on about 40 candidates and stage F on the survivors over 5 frame sets [G]; the 6-geometry and out-of-family runs are `fit-matrix`'s, 8.5); the goldens binary + ≤ 7 s (M3.7); the lock ≤ 10 s (M3.8); schema, edits
and agents ≤ 3 s (M3.9); app kittests ≤ 10 s (M3.10, M3.13); export parity and controls ≤ 15 s
(M3.11); LRV ≤ 5 s; exposure and profiles ≤ 3 s each; GoPro's synthetic EAC rungs ≤ 10 s (M3.18) and the caps' rows and parity ≤ 3 s (M3.20) [G]. M3 adds at most about 135 s to `cargo xtask
check`; each slice note records the growth.

## 20. Dependencies and provenance

- **No new third-party crates.** The protobuf walker, the GPMF header walk and the
  Levenberg–Marquardt solve are hand-written; the fit's threads use `std::thread::scope`; `rig-fit`
  uses `clap` and the caps use `image`, both already in the workspace. `deny.toml` doesn't change
  and gains no git source. libx265 lossless comes from the FFmpeg we link.
- **Inside the workspace** (D22's rows and D28's layout updated in the slice that makes each
  change): `spherecut-camera` (insta360, later gopro and dji, `known`, `profile`) depends on core
  and sphere, and media on it; sphere gains `fisheye::Unified`, `KbLens` and `rig`; render gains
  `rig`, `seam` (signature, fit, gain), `SourceKind` and the weighted compare; media gains
  `track_timing` and `stss`; core gains `project::source` and `dirs`; cli gains the `fit` feature.
- **telemetry-parser is not used in M3** (D62): neither for the X5's trailer nor for DJI's
  calibration, not even as an oracle. PLAN's "via telemetry-parser where it fits" is clarified in
  M3.0, and M4 still decides whether it parses the IMU (DECISIONS.md's Open item, updated to say
  so). **OpenOSV**'s `proto/dvtm_osmo360.proto` (Apache-2.0; its NOTICE is kept if any code is
  ported) gives 18.2's field numbers, never telemetry-parser's DJI protos or `rig.rs`.
  **max2sphere** is built out of tree as a reference renderer for GoPro only.
- **Used with attribution:** Gyroflow (GPL-3.0-or-later) for the inverse's method; the Mei paper and
  OpenCV omnidir for the model; max2sphere (Apache-2.0) and gpmf-parser (Apache-2.0 or MIT) for the
  EAC geometry and the KLV layout; FFmpeg's `v360` for the YouTube EAC layout that synth writes
  (18.1); nadir-patcher (Apache-2.0) for the cap's radial mapping (its orientation is ours, 18.3).
  As facts and ideas, never code: GyroView (MIT) for map C and its V6 reading; raw360-formats (MIT)
  for map B and the same reading; orbis-reframe (GPL-3.0-or-later; its `src/insv/` MIT OR
  Apache-2.0) for map A and its own V6 reading. **Never used:** rule 7 of 4.1.

## 21. Risks, most serious first

| # | Risk | Retired or contained by |
|---|---|---|
| 1 | **The geometry conventions are wrong, or can't be settled.** A 1.2 % focal error is 41 px at the seam at 8K, V3 against V6 8 to 17 px per lens and the V6 reading 12 to 33 px [C]; each looks like a bad stitch, clip 8 can't tell the scale or a tilt from parallax, and twins differ by a turn the seam cannot see (6.2). | *Retired by* the attribution matrix and the planted-truth recovery with hard neighbours and with truths whose expected verdicts are open or fitted (M3.4, M3.6, `fit-matrix`), which prove the fit tells them apart where the take determines them and says so where it does not; the provisional call on clips 8 to 10 (M3.6); take 14 for scale and the floor and take 15 for the side (M3.12), under rules fixed in advance. *Contained by* geometry as data (no code waits on an answer), equivalence classes, `Extrinsics::Fitted`, and the redirect to an own calibration (8.7), which needs no new feature. |
| 2 | **The instrument is wrong**: a biased estimator, or synth and sphere sharing a convention bug. | *Retired by* independent code for the lens model, the extrinsics and the strings (4.1 rule 2), the self-tests against pure resampled shifts, prediction against measurement for every control, the cross-scene condition, the outside checks (8.8), and the real-footage floor measured before any gate. |
| 3 | **Takes 14 and 15 come late or can't be used.** | *Contained by* the provisional entry from clips 8 to 10; only M3.12 waits for the takes; the usable-take rule turns a bad take into a precise re-take request. |
| 4 | **The seam tears, or the lock freezes it.** `pick`, upload, catch-up and the end are per stream today [F], and an all-or-none present on top of them would freeze the view whenever a lens lags (9). | *Retired by* P-TRK before the lock, the pure `pick_pair` and its property, the group base and the group `until` (9), the content readback with its lock-off control, the pacing test with its control, the slot floor, P-LOCK on odin with 8K25, and the content-based offset test (k = 0 by 5σ). *Contained by* the `pair_resync` backstop, and the raw view when a lock is refused. |
| 5 | **Decode throughput.** The 4K30 export from 8K30 raw needs about 1.8× today's decode [G]; on odin VA-API download decodes the 8K30 pair at 22 to 31 fps a lens and Vulkan Video at 58 to 61, from perf.md's runs (K1) [C]; huginn has VA-API only, at 1.10× to 2.40× real time for that pair (33 to 72 fps a lens), unstable, and has never run an export or the engine on a pair. | *Retired early on odin by* K1 in M3.1 and its ladder (Vulkan Video download, then M3.11b). *On huginn, retired when its answer comes* by K2 (path A by D25's revisit rule) and K4 (M3.11b's path A row); until then its export and 5.7K30 rows read "pending huginn". If a target still misses, Pat gets a decision card with the stage times and options (section 12). |
| 6 | **Memory.** The export holds 10 decoded lens frames at 8K, 221 MB [C] (two pairs in the channel, each decoder's current and next frame, the pair being rendered), before FFmpeg's own frames and the download pools; slots take 177 MB; path B's pool may reach about 707 MB; D42's slump doubles. | *Retired by* K3 against its one-lens baseline, with its levers in order (the software fallback's frame threads, the download pool's size, `DECODED_AHEAD`, then M3.11b), and the RSS rows in M3.1, M3.11 and M3.17. |
| 7 | **Projects made under a provisional geometry outlive it.** | *Retired by* status and evidence in the project, the comparison on load (11.4), the notice and its edit, `check`'s rule, render's warnings, and the test that an old project renders the same bytes until Pat accepts. |
| 8 | **Exposure matching breaks "export matches preview".** | *Retired by* one zero-phase track stored in the project and read by every renderer, the parity test with its causal-filter control, and the stale basis. |
| 9 | **Mip bleed** at the black corners and between the LRV's halves. | *Retired by* the margin that grows with λ, one slot per half, and `dfish_mask_margin`, `dfish_lod_heatmap` and `lrv_packed_vs_pair` with their controls. |
| 10 | **Cross-crate slices overload agent teams.** | Slices of one or two layers each (section 22), interfaces fixed in a slice's first commit, a reviewer per unit, and a fresh review after any slice over about 3,000 lines. |
| 11 | **Tier 1 cost and the cloud disk.** | Per-suite budgets (19.6), printed wall times, temporary synthetic media under 50 MB, the costly fit matrix behind `xtask fit-matrix`. |
| 12 | **Provenance.** | Rule 7 of 4.1 in every review, our own generic Euler space, a Studio export only ever compared against. |
| 13 | **No GoPro or DJI samples.** | The synthetic EAC round trip, "done pending samples", DJI's finding with its stitched-export path; nadir moves first if samples are missing. |
| 14 | **The shader's cost.** | P-SHADE, with the analytic Jacobian as the redirect. |

## 22. Slices

### 22.1 The table

Each slice branches from main, ends with the push rule (4.1 rule 6) and merges with `--no-ff` on
its own. **HW**: odin by Remote Control, or huginn by a prompt Pat pastes (nothing waits for
huginn's answer: its rows read "pending huginn" until it comes, and go into perf.md whenever it
does, 4.4). **Samples**: "the 14" are Pat's files in the cloud; take numbers are SAMPLES.md's 14 to
17. Clips 1 to 4 are files 004 to 007 (8K30, 8K25, 5.7K60, 4K120) and clips 8 to 10 are files 008
to 010 (`docs/formats/insta360.md:21-29`) [F]. The units (a, b, c…) are one agent's work each, with
one reviewer each.

| Slice | Delivers (units) | Depends on | Done when | HW | Samples | Pat sees |
|---|---|---|---|---|---|---|
| **M3.0** Facts first | (a) `track_timing`, the `stss` reader, `samples_x5_tracks_equal` (P-TRK and its LRV part); (b) the dropped-project fix (13.1); (c) `testkit::control::must_fail`, `testkit::tier2::require_samples_cpu()` (Tier 2c's gate, needing only `SPHERECUT_SAMPLES`; 4.4) and `samples-roles.toml` (with take 14's tripod height once Pat's note gives it, which 8.4's ground model reads), as M3.0's first PR, merged on day one; (d) the corrections of 5.4, PLAN's telemetry-parser line (20; clarified, not amended), DECISIONS.md's telemetry-parser Open item updated to "M3: not used (D62); M4 still decides IMU parsing" (SAMPLES.md's takes 14 to 17, with clip 12 conditional on question 2 and take 14's vertical edges, second heading and tripod height, are committed with this plan) | none | (c) merged before M3.1, M3.2 and M3.3 begin; P-TRK's verdict per field in PROGRESS, with the branch taken; the bug test fails on main and passes; `samples_x5_tracks_equal` passes in the cloud by 4.4's command with `SPHERECUT_TIER2` unset, and `require_samples_cpu` skips when `SPHERECUT_SAMPLES` names a missing folder (its control) | none | the 14 | an agent's `.insv` project stays open (raw view) |
| **M3.1** Pair decode | (a) `synth::code` corner codes; `mux_video_streams`' MP4 output, which keeps each track's time base; the two-track MP4 perf clips, 600 frames each as M2's are (`crates/synth/src/media.rs:882`) and coded per mode as 7.2 says; the 2×64² two-track test files with a length and a GOP per track; (b) `bench_export_pair_decode` (serial and per-lens threads, three paths; the gated rows beside a VA-API encode by K1's rule; RSS against a one-lens run, each run in its own child process, 4.3), and rows for the two-track perf clips in `bench_playback_x5_raw` (K2's engine half); (c) huginn's prompt as one paste (4.3) | M3.0 (c) | K1 and K3 verdicts on odin in perf.md with the branch taken; D52's update drafted (K1 expects Vulkan Video download to win on odin); M3.11b opened or closed on K1; each perf clip's per-track bitrate, IDR interval and NAL types in perf.md beside the masters', with the too-easy flag per track; huginn's prompt sent, saying how long the perf clips take to make. It merges on odin's numbers alone: K2 and K4 go into perf.md whenever huginn answers (D25's revisit rule applies to K2, and M3.11b reopens for huginn if K4 or K2's export half fails), and M3.17 takes them up if they haven't come | odin; huginn by prompt, not waited for | 1 to 4 (8K30, 8K25, 5.7K60, 4K120) and 9 (K1's gated row) | huginn's one-paste prompt, with about two hours of making clips before its benches |
| **M3.2** Camera crate | (a) trailer, index, records and limits (5.1); (b) the walker, metadata, strings parse and print (5.2); (c) `companions`, `verify_companion`, probe, doctor `--file`, `cli-json.md` (5.4); (d) synth's own trailer writer and string printer from numbers | M3.0 (c) | 5.5's properties at 100k; the cross parse; every trailer fault a clear error with its offset; both Tier 2c tests on the 14; probe's contract test, which also pins the old shapes (the CLI's `camera` object keeps `layout` and `evidence` beside the new `serial`, and the agent's summary `camera` stays a string beside the new `camera_detail`); each camera reader (`read_trailer`, `read_record`, `verify_companion`) panics on a thread marked as the UI thread (5.1); clips.rs's tests unchanged | none | the 14 | `spherecut probe` names the camera, serial, firmware, lenses and calibration |
| **M3.3** Lens maths | (a) `Unified`, `Canonical`, V3's reading and `V6Reading` (21 values), the inverse, `limit_theta`, `KbLens`; (b) `sphere::rig` (6.2) with `rig::theta_frame` (6.1); (c) `synth::lens`, `synth::lens::compose` with its hand-worked unit tests, `RigTruth`'s ground plane, per-lens time offset and rolling-shutter readout, the per-binary render cache (7.1), and `SeamLines`; rung 0 | M3.0 (c) | 6.3's properties at 100k in ≤ 30 s; rung 0 on every truth model; all 1152 `EulerConv` values and all 21 `V6Reading` values distinct; `euler_twins_are_quarter_turns` (6.2): the twin groups for sampled stored angles, whose common rotations are exactly the quarter turns about Z once stage G's −Z filter is applied; synth's self-tests, `compose`'s hand-worked values among them; `Rig::build` agrees with `compose` (6.3's Against synth) | none | none | nothing |
| **M3.4** The instrument | (a) weighted metrics (8.1); (b) `reference::render_rig` with debug modes 4 to 7; (c) strips, shifts, signature and self-tests (8.2); (d) the attribution matrix (8.3); rung 1 with every control | M3.3 | each control fails and is attributed to the term 8.3's step 1 predicts (prediction within 0.05 px rms, crosstalk under its bound); the common-mode control passes every seam check and fails the comparison with the ideal (4.2); rung 1 within 4.2's thresholds; 3.4's budget matches the images; within 19.6's budget (≤ 15 s) | none | none | nothing |
| **M3.5** The file | (a) `write_insv` (N-track `Mp4Writer`, LRV, faults); (b) `to_calibration`, `known` with the placeholder entry, probe's geometry blocks; (c) `Still::open_pair` and `frame <insv>` on the CPU reference; rung 3's CPU half | M3.2, M3.4 | rung 3's planes hash equal and its render equals rung 1's; each trailer `InsvFault` (`TruncatedTrailer` to `VarintOverflow`) a clear error naming the record (5.5); ≤ 3 s; `frame` of clip 8 renders in Tier 2c | none | 8 | nothing new (placeholder geometry) |
| **M3.6** The fit, provisional | (a) `render::seam::fit` (8.4); (b) `rig-fit`, `xtask fit-matrix` (from then on part of 4.1 rule 6's push rule for any slice that touches `render::seam`, `sphere::rig` or `synth::lens`); (c) `fit_recovers_planted_geometry` (8.5); (d) the run on clips 8 to 10: `known`'s provisional X5 entry, `samples_lens_offset_is_zero`, `samples_thumbnail_orientation` if Pat agrees, P-GAIN's first numbers | M3.5 | 8.5's expected verdicts met in Tier 1 and in `fit-matrix`, the redirect and out-of-family truths included; `docs/research/m3/fit-clip{8,9,10}.md` with σ, the ranking, each question settled or open, the directions stage F leaves undetermined, and the fine class open on clips 8 to 10; the X5 entry written from the run whatever the rules decide (the decisive value, the redirect's, or the best candidate as "best of N, not decisive", 8.6), the placeholder kept, with `stitchable: false`, only if clips 8 and 10 fail the gross rule; k = 0 by 5σ or the offset recorded; D64 and D65 drafted | none | 8, 9, 10 | **the first stitched stills of clip 8** (`spherecut frame`), with the provisional numbers; if the gross rule fails, a note that says so and what the design note will look at |
| **M3.7** Stitch on the GPU | (a) `fs_rig` and its structs (10.3); (b) the key, group 3, the rig uniform and each `LinearTarget`'s `rig_group` (`layouts::view_rig`), group 2's `layouts::sampler_caps` with `Projector`'s caps-off groups, `SourceRef`, `pack` with the frame's gains, and `StillRenderer`'s pair through `render_from` on all three upload paths (`SourceFrames`, `LensFrame`, `PlaneSource`) (10.1, 10.2, 10.4); (c) the `dfish_*` goldens, rung 2; (d) `frame` and `look` on the GPU through `agent::still`'s pair path (two `copy_out`s under path B, two imports under `--zero-copy`, the bytes otherwise, then `render_blurred`), and frame's hints (14); (e) RADV goldens and P-SHADE | M3.4; merges after M3.5 (rung 3's GPU half, `frame` and `look`) | the 24 old goldens byte-identical; each new row and control on lavapipe, three identical runs; the naga interface test; `a_pair_renders_alike_on_every_upload_path` byte-identical on lavapipe, with its swapped-buffers control differing; RADV goldens and P-SHADE's verdict on odin, and `zero_copy_pair_frame_matches_the_copy` under VA-API (DMA-BUF) and Vulkan (path B) | odin | none | a stitched still in about a second; agents' `look` at an `.insv` (shown to Pat under M3.6's geometry) |
| **M3.8** Frame lock | section 9: (a) `MediaBackend::track_timing` (`FfmpegBackend` from P-TRK's MP4 tables, `None` otherwise), the open checks, `LockRefused` and `LensShort`; (b) `pick_pair`, `unpaired_presents` over every stream, `pair_wait`, `pair_resyncs`; (c) the group base in `FrameCache` (`set_group`, Tg as the members' playhead), the group `until`, group seeks and the end, the resync backstop; (d) slots and their floor of 2 per member, per-lens fallback; the `PerStream` test backend and the readback tests | M3.0 (`track_timing`); M3.1 (`mux_video_streams`' MP4 output, the corner codes, the two-track MP4 perf clips and the 2×64² test files) | `pick_pair`'s property at 100k; `a_locked_pair_never_shows_mismatched_frames` at 0 unpaired by tag and by content, its lock-off control above 0 on both; `a_locked_pair_paces_to_its_slowest_lens` meets the pacing rule in every case and its control fails; each planted fault refused by name, and the one-frame-short lens locked and ended at the shorter lens with one `LensShort`; the slot floor unlocks within `WAIT` and its no-floor control times out; the fallback test keeps lens 0 on hardware with one notice; old two-stream tests pass; on odin 0 unpaired by tag and by corner code, `zero_copy_pair_matches_copy` (9), and P-LOCK's verdict (8K25 on path B included) with the slot count | odin | 1, 2, 3, 4 (P-LOCK's 8K30, 8K25, 5.7K60 and 4K120) and 9 (real-time play), and M3.1's MP4 perf clips | the raw view never shows the lenses at different frames, and a lagging lens slows the pair instead of freezing it; `unpaired` and `pair_wait` in the overlay |
| **M3.9** Schema 2 | section 11: (a) the model, migration and `validate`; (b) the three ops, their inverses, examples and core's per-op samples (`core/tests/project_edits.rs:1095`); the reference project's optional camera media 11, a synthetic `.insv` the agent test writes and probes; agent `edit()`'s server-side expansion of `set_source`'s camera form and `wired()`'s filter for it (11.3); 11.4's comparison in `check`, `render` and the summary; (c) `project_media`, 11.5's export and viewer fixture, agent `project create`, the `not_yet` refusals | M3.5 | both fixtures; the edit property at 100k; `validate` at each bound; the missing-`source` refusal; `project_create_x5` (Tier 2c) gives the trailer's hash; D57's example test with 24 ops, the three new ones applied to media 11 and undone exactly (`set_stitch`'s example setting both a tune and gains), `set_source`'s camera form read from the file (11.3); an old project renders the same bytes under a newer (fake) table entry | none | 8 | agents' projects of `.insv` files record camera, calibration and geometry; agents tag a file as equirect with `set_source` |
| **M3.10** Stitched in the app | 13.2, 13.3 without profiles, 13.5, 11.4's notice: (a) open flow, `Doc.rig`, `DrawKey`, "Stitch it" and "Treat as equirect" (`set_source`, 18.2), and a `Known` entry with `stitchable: false` shown as lenses with a notice (5.3); (b) viewer, overview, Raw lenses, the lock's notices (`LensShort`, the slot-floor unlock); (c) the Stitch pane; (d) kittests and smoke, with the perf clips as `.insv` files (synth's trailer appended, 7.2) | M3.7, M3.8, M3.9, M3.1 (the 2×2880² perf clip `bench_playback_x5_stitched` gates on, 19.4); what Pat sees waits for M3.6 | rung 5 within 1 LSB on 11.5's fixture, its `Tune::default()` control missing; 13.5's kittests, among them "Treat as equirect" in one undo step and the `stitchable: false` notice; on odin `gui_smoke` on clip 8 at 8K30 ≥ 99 % presented with UI p99 within refresh, and `bench_playback_x5_stitched` (19.4); seam screenshots in the note, under M3.6's provisional entry (if M3.6 hasn't merged, the note says "placeholder geometry" and the screenshots wait for it) | odin | 1, 3, 8, and M3.1's 2×2880² perf clip | once M3.6 has merged, **clip 8 plays stitched** under its provisional geometry; per-lens numbers in the Stitch pane |
| **M3.11** Export from the pair | section 12: (a) decode, `Held`, the zip and `pairs_ahead` (D56's lead counted in pairs); (b) every project renderer takes the pair, `not_yet` gone; (c) parity, controls and benches; (d) the end-to-end note | M3.7, M3.9, M3.1's K1 verdict; merges after M3.10; its note (d) waits for M3.6 | rung 4 flat (on a side seam) and 360 on 11.5's fixture with its four controls (lens 1 late, both late, the tune dropped, gains off on the Auto twin); an export of an `InsvFault::LensOneFrameShort` file ends at the shorter lens with the viewer's warning ("lens B has N fewer frames"), on the frame the player ends on; the app's `the_export_yields_while_a_locked_pairs_lens_lags` with its per-slot control; on odin `stitched_export_matches_preview` at M2's VA-API floors (47.7 / 50.1 dB) with the frame-late and planted-gain controls, and `bench_export_4k30_from_insv` over the whole export from 5.7K30 (600 frames) and 8K30 (all of clip 8), where a miss goes to Pat as a decision card with stage times; huginn's rows read "pending huginn"; **the note to Pat**, under M3.6's provisional entry: commands, what to look at, the geometry's status, limits | odin; huginn when it answers | 1, 8 | once M3.6 has merged, **clip 8 exported stitched**, flat and 360, from the window, the CLI and agents |
| **M3.11b** (only if K1 fails, or K4 or K2's export half when huginn answers) | the export decodes onto its own device through path B or A (4.3), each lens's planes reaching `render_yuv`'s `Pair` as `PlaneSource::Buffer` or `Imported` (10.4) | M3.11 | section 12's done-when with the decode stage under 25 ms a pair at 8K30 on odin; where K4 or K2's export half failed, huginn's path A row at ≥ 30 fps, reading "pending huginn" until its paste returns | odin; huginn in that case | 1, 8 | the 8K30 export at real time |
| **M3.12** Geometry settled | `rig-fit` on takes 14 and 15 under 8.6's rules (clip 12 compared only if Pat agrees); `known` settled or redirected (8.7); `screen_side_track`; seam rows gated; insta360.md updated | M3.6; takes 14 and 15. Nothing waits for it: until they come, PROGRESS lists it as waiting on them, the X5 entry keeps M3.6's provisional numbers and the `seam_quality_*` rows stay recorded | each row of 8.6 settled by its rule or written up as open with its numbers; `seam_quality_take14` and `_clip8` gated at the floor + 3σ, σ as 8.4 defines it; `samples_binding_take15`; D64 final; a short review of its own, since it may come after M3.21 | none | 14, 15 | the badge reads "measured"; lenses named front and back; one notice on older projects |
| **M3.13** Fine-tune | 13.4: (a) Alt-drag and the q drag; (b) Measure seam and Estimate distance as jobs; (c) `look mode: "seam"`, the agents' recipe, `docs/agents.md` | M3.10 | the drag is one undo step, keeps its point within 0.5 px and shrinks the signature; Measure seam and `look seam` report a planted 0.5° roll as 0.5° ± 0.05°; Estimate distance finds q within 0.05 m⁻¹ at R = 1.5 m; the recipe replays within budget; `estimate_distance_stands_aside_while_playing`: no frame decoded while the viewer plays, Cancel ends it at once (13.4); odin's `gui_smoke` with a seam drag | odin | none | **a seam number with its dominant term and a hint**; drags the seam |
| **M3.14** LRV proxies | section 15: (a) `render::upload::Columns`, `Columns::buffer_planes` and `upload_from_buffer`'s end check at wgpu's own rule; (b) the proxy's lock group: `LockSpec.columns`, the upload pass's per-member columns, VA-API's download for a member with columns, and `a_packed_proxy_never_shows_its_halves_apart`; (c) companions in the app, the time map, the `proxies` setting and toggle, rung 6 through the viewer, `lrv_packed_vs_pair`; (d) the Tier 2 rows (`lrv_matches_master` recorded, `zero_copy_lrv_matches_copy`, the seek and drop benches on path B) | M3.10 | rung 6 through the app's viewer with its control; `lrv_packed_vs_pair`; the frame-map and `Columns` properties at 100k; the `Columns` upload test and `shared_vulkan.rs`'s packed case byte for byte, each with its control; `a_packed_proxy_never_shows_its_halves_apart` at 0 unpaired and its lock-off control above 0; `never_blocks` with an `.insv` beside its LRV and the proxy toggled on and off, backend gated and harness thread marked, with its `read_trailer` control (13.5); an export with proxies on reads the master; `lrv_matches_master`'s median SSIM and σ per pair recorded with both controls, then the row gated at the lowest median less 3σ in a reviewed change; on odin, on path B (D42), `zero_copy_lrv_matches_copy`, the stitched LRV seek p95 < 150 ms and 0 dropped | odin | the 7 LRVs | 8K clips scrub on the LRV; a proxy toggle |
| **M3.15** Exposure match | section 16 | M3.10, M3.11 | the estimator's properties at 100k, the ramp against the truth through the same filter with its causal-filter control (16); `dfish_exposure_match`; export parity with Auto and its causal-filter control; the stale-basis test; `analysis_stands_aside_while_playing` (13.5); an agent's Auto analysis runs as a D58 job that ends in one "Exposure match" edit, never inside `edit`; `exposure_step_after_match` recorded on clip 9 (gated on take 17 if it comes); P-GAIN's default | none | 9 (17 if it comes) | no colour step at the seam, in preview and export; the slice note gives P-GAIN's numbers on each take and the exposure default they set (Off or Auto) |
| **M3.16** Profiles | section 17 and agents' `apply_profile` | M3.10 | section 17's tests; `skill.rs` on `core::dirs` with its tests unchanged | none | none | saves the tune for Pat's X5; new clips open with it |
| **M3.17** X5 hardware pass | the full hw-test on odin; huginn's prompt; take 16's rows; `gui_smoke` on clip 8 stitched with an Auto analysis pending (13.5), at ≥ 99 % presented and no more frames dropped than the run without it; perf.md; 19.5 filled; Pat's eye-check list; PLAN's X5 ticks | M3.10 to M3.16 except M3.12 | every Tier 2h row on odin passes or is reported with its number and stage; huginn's rows are in perf.md with K2's and K4's rules applied, or read "pending huginn" with the date its prompt went out; neither blocks M3.21. Without take 16 the 5.7K30 row is gated on the synthetic 2×2880² clip alone, and without take 17 the exposure gate stays recorded on clip 9 (question 1) | odin; huginn by prompt | 16 and 17 if they come (optional) | the performance table and an eye-check list |
| **M3.18** GoPro MAX | 18.1 | M3.7, M3.8, M3.9, M3.10, M3.11 (rung 4 is the export; the app shows `.360` files) | synthetic EAC rungs 1 to 4; `eac_*` rows with controls; with samples, max2sphere's floor and the lock tests; else "done pending samples" | odin (goldens) | GoPro set (optional) | `.360` files stitched, given samples |
| **M3.19** DJI Osmo 360 | 18.2 | M3.6, M3.7, M3.9 | with samples, `camera::dji` (OpenOSV's field numbers, `known` starting on `native_refine`), a provisional entry and the fit on an open-field take; without, dji.md's M3 state and the stitched-export test (probe's tag, then `set_source {"kind": "equirect"}` through agent `edit`) | none | DJI set (optional) | DJI raw stitched (in the app once M3.10 is in), or its stitched export tagged ("Treat as equirect", M3.10) |
| **M3.20** Nadir and zenith | 18.3, with each media's four `layouts::sampler_caps` groups made when its PNG loads and passed through `SourceRef` (10.2) | M3.7, M3.9, M3.10, M3.11 (parity with a patch is an export) | `nadir_fill`, `nadir_patch` (its glyph at both poles, with the mirrored control) and `zenith_fill`, with controls; parity with a patch; `set_caps`'s example and inverse | odin (goldens) | none | the tripod hidden by a fill or a logo |
| **M3.21** Docs and review | D60 to D76 and the updates (23); ARCHITECTURE, PROGRESS, README, `cli-json.md`, agents.md, the format notes; a fresh review in four areas (camera and maths; render and the instrument; playback and export; app and agents) that reads the slice notes as well as the code; the fixes; the note to Pat | all but M3.12 | the review's findings fixed or listed as known; PLAN's ticks, with the X5 geometry listed in PLAN and PROGRESS as waiting on takes 14 and 15 if M3.12 hasn't run | none | none | M3 done, or done pending samples (takes 14 and 15 for the X5's geometry; GoPro's and DJI's sets) |

### 22.2 Order

```
day one    M3.0 (c): must_fail and the Tier 2c gate, merged before M3.1 to M3.3 begin
start      M3.0 facts     M3.1 decode     M3.2 camera     M3.3 maths
              │               │               │               │
              ├───────────────┤               │            M3.4 instrument ───────┐
              │               │               └──── M3.5 file (M3.2 + M3.4) ┄┄┄┄┄┄┤
              │               │                     │        │                    │
           M3.8 lock          │                  M3.6 fit  M3.9 schema 2      M3.7 GPU stitch
              │               │                              │                    │
              └───────────────┼──────────── M3.10 app (M3.7 + M3.8 + M3.9 + M3.1) ┘
                              └──────────► M3.11 export (K1; merges after M3.10): end to end
           M3.6 ┄┄► what Pat sees at M3.7, M3.10 and M3.11, and M3.11's note
then       from M3.10: M3.13 fine-tune · M3.14 proxies · M3.16 profiles
           from M3.11: M3.15 exposure · M3.18 GoPro · M3.20 caps
           M3.19 DJI (M3.6 + M3.7 + M3.9)
whenever   M3.12, once takes 14 and 15 are in and M3.6 has merged; nothing waits for it
last       M3.17 hardware pass, M3.21 review (all but M3.12)
```

M3.0's unit (c) merges on day one; after it the four starting slices share no code. M3.8 hangs
from both M3.0 (`track_timing`) and M3.1 (the MP4 mux, the corner codes and the perf clips), and
M3.10 also takes M3.1's 2×2880² perf clip. The longest chain to the end-to-end point is M3.3, M3.4,
M3.5, M3.9, M3.10, M3.11. M3.7 runs beside M3.5 and M3.9 and merges after M3.5; M3.8 runs beside
the rest once M3.0 and M3.1 have merged. The dashed lines are orders, not code: M3.7 waits for
M3.5's file, and what Pat sees from M3.7, M3.10 and M3.11 waits for M3.6's provisional geometry,
so the code of those three may merge first, with "placeholder geometry" in its note. At most
three slices are in review at once. M3.20 moves before M3.18 and M3.19 if GoPro or DJI samples
are missing. Without takes 14 and 15, M3.21 closes M3 as "done pending samples", and M3.12 runs
with its own short review when they come.

### 22.3 What each slice fixes first

A slice's first commit fixes the types its units share, with `todo!()` bodies and the tests'
names, so the units can work at once: the code blocks of its sections. For example: M3.0's unit
(c) is its own first PR, so M3.1 to M3.3 start with `must_fail` and `require_samples_cpu`; M3.3's
`Unified`, `Geometry`, `EulerConv`, `V6Reading`, `Tune` and `Rig::build`, with
`compose`'s signature (M3.3's `Tune` holds no gains, so M3.3 needs nothing from core); synth's `Planted`, which M3.2's `print` (7.2) also takes, in M3.0 (c)'s day-one PR, so M3.2 and M3.3 both start with it; M3.7's
`SourceRef`, `RigUniform` (offsets naga-checked), `layouts::view_rig` and `layouts::sampler_caps`,
`SourceFrames` (`One(LensFrame)` or `Pair { lenses, rig, gains }`) with `LensFrame` and
`PlaneSource`, the new signatures of `render_from`, `render_blurred` and `render_yuv`, and
`pack(&Rig, [SlotGeom; 2], gains)`; M3.8's `LockSpec` (with its test-only switch for the group
base), `pick_pair`, the `MediaBackend::track_timing` signature with no default body, forwarded by every wrapper backend and answered by `SyntheticBackend` from its clip (9),
`PlayerEvent::LensShort` and the `PerStream` test backend, whose frame cost can be set for each
decoder it opens as well as for each file stream, so that M3.14 can slow one of two members over
one stream (M3.14 adds `LockSpec.columns`); M3.9's `Source` JSON with `DualFisheye.gains`, `Gains`
with `Gains::at`, `AnalysisInput`, the 16-hex-digit hashes (from core's `fnv1a64`, which M3.2 makes public, since its probe prints the calibration hash, 5.4), and the
ops' wire forms; M3.6's `FitReport` JSON and 8.6's rules as one `verdict` function, which always
returns an entry (8.6). A reviewer per unit checks against the done-when and rule 7 of 4.1. A slice
whose diff passes about 3,000 lines gets a fresh review before it merges.

## 23. Decisions to record

| D | Choice | Reason | Slice |
|---|---|---|---|
| **D60** | **M3's verification contract**: 4.1's rules; the round-trip ladder; every check with a control that must fail; independent instruments; floors before gates; Tier 2c in the cloud, gated on `SPHERECUT_SAMPLES` alone (`require_samples_cpu`); the slice note in place of a per-PR ledger | Conventions are M3's main risk, and they fail as "a bad seam". Only a measured number with a control and a floor tells a guess from a fact. The slice note carries the same rows without a second document to keep in step | M3.0 |
| **D61** | **Crates**: `camera` parses vendor bytes, with no FFmpeg; media depends on camera, never the reverse; lens maths and the rig in `sphere`; blending, the instrument and the fit in `render`; no `stitch` crate. Mei goes beyond PROMPT.md:79's "equidistant/equisolid plus polynomial". Updates D22, D28 | synth's checks, the reference, the fit and the project share the maths, and sphere already holds every projection. render holds the reference and builds at opt-level 2. A stitch crate would hold nothing of its own. The X5 writes Mei | M3.2, M3.3 |
| **D62** | **Our own trailer reader**: a hand-written varint walker, D15-style limits, `impl Read + Seek` inputs; every reader calls `core::blocking::may_block`, and the app reads only on its jobs; `companions` works on names only. telemetry-parser is not used in M3, not even as an oracle: neither for the X5's trailer nor for DJI's calibration block, whose field numbers come from OpenOSV's `proto/dvtm_osmo360.proto` (Apache-2.0). PLAN's line is clarified, not amended. M4 still decides whether it parses the IMU (DECISIONS.md's Open item, updated). Updates D15 | deny.toml forbids git sources; crates.io's 0.2.x predates the X5; its geometry is Studio-derived, and its DJI `rig.rs` takes the `far_11` default and the 0.038 blend from hooking DJI Studio (dji.md §11.2). The records' footers, the index and the byte-identical reprint check our walk on every file | M3.0, M3.2, M3.19 |
| **D63** | **The lens model**: `Unified` with one 13-term canonical distortion that every V3 and V6 reading maps onto (V6's d5..d12 through a generic `V6Reading` space of 21 values); `KbLens` with k1..k5 on its base, applied after the polynomial and carried per lens to the shader; forward only in WGSL; an inverse on the CPU that repeats strip-and-bracketed-Newton until the residual is under 1e-12, at most 20 passes; `limit_theta` bounds θ_max; `theta_frame` bounds the feather | It is what the camera writes. One canonical form makes the GPU, the CPU and the fit agree under every reading. The fixed-point inverse fails from about 74° on the X5, and three passes leave up to 2.1e-6 rad on Pat's numbers [C]. A reading named after a source would favour it. The X5's frame cuts its circle at about 99° to 103° [C] | M3.3 |
| **D64** | **Geometry is data**: `Geometry`'s fields with status and evidence; a generic Euler space of 1152 values with no direction field; twins (6.2) reported as one class, `upright` a quarter turn about −Z set by the up-and-down rule, and stage G keeping only values that put block 0's lens within 5° of −Z; a generic V6 space of 21 values; synth composes the extrinsics with its own code (4.1 rule 2); `Known` by model and firmware, provisional for unlisted firmware; M3.6 always replaces the placeholder with what clips 8 to 10 show (8.6), and keeps it, with `stitchable: false`, only if they fail the gross rule; projects store theirs, and only an edit changes it | Seven things are unknown and the readings in the wild conflict. Our own spaces keep provenance clean, and two compositions written from 6.2's text check each other. Storing keeps old projects' bytes. A placeholder that outlived the fit could show Pat a lens upside down | M3.5, M3.6, M3.12 |
| **D65** | **The seam signature and the fit**: strips through the CPU reference; NCC then Lucas–Kanade per 5° bin with its information; the harmonic terms; the attribution matrix; stages G, F and J with equivalence classes and the decisiveness rule; stage F on frames at rest with its information matrix, the undetermined directions reported and judged on nothing; take 14's ground model; σ as the larger of the frame-set spread and the information matrix's standard error; 8.5's expected verdicts as `must_fail` controls, and `fit-matrix` in the push rule; 8.6's rules fixed before the takes; the redirect to an own calibration; a Studio export never fitted to. One implementation serves the tests, `rig-fit`, Measure seam and `look seam` | Each term maps to parameters and Tier 1 proves the mapping. The numbers Pat and agents see are the tests' numbers | M3.4, M3.6 |
| **D66** | **Stitching in the projection pass**: an entry point per source kind (`fs_rig`, `fs_eac`, `fs_project_caps`), not an override; the rig uniform in a second group-0 bind group (`layouts::view_rig`: the view uniform at 0, the rig at 1) that each `LinearTarget` owns beside `view_group`; the cap texture and the caps uniform in group 2 beside the sampler (`layouts::sampler_caps`), in four bind groups the media's caps own, with `Projector`'s caps-off defaults; lens B in group 3; `StillRenderer`'s `render_from`, `render_blurred` and `render_yuv` take each lens's frame and upload path (`SourceFrames`), and `render`, `render_imported` and `render_copied` stay one-frame wrappers; per-lens LOD from projected rays; a mask margin that grows with LOD and a frame term that keeps every footprint inside its slot; a θ feather with h = 5°, at most min(θ_max, θ_frame) − 90°; q stored and metres shown; gains in linear light; no intermediate equirect. Updates D31, D32 | PROMPT.md:65. wgpu-core 30.0.1 builds an entry point's interface before overrides exist (10.1), so an override would make every equirect pipeline bind the rig. A bind group fits only pipelines whose layout matches it, and a cap belongs to the media, not to the targets that draw it. The X5's square frame cuts its image circle at 99° to 103° on Pat's numbers [C], inside the reach of the strips and a wide feather. An 8K RGBA16F intermediate is 225 MiB a frame. q is linear in disparity and 0 is ∞ | M3.7, M3.20 |
| **D67** | **Frame-locked pairs**: timing through `MediaBackend::track_timing` (MP4 tables; a track without them refuses the lock); open checks on time base, start, rate, size, and `stts` and `stss` over the common prefix, not the count (the pair ends at the shorter lens with one notice); `pick_pair`, all or none; the group base Tg as the members' cache playhead; one group `until`, never past what the other members hold; the `pair_resync` backstop; 6 slots (8 after P-LOCK) with a floor of 2 live slots per member, below which the group unlocks; per-lens fallback; `unpaired_presents` over every stream, `pair_wait`, `pair_resyncs`. A group may hold two members over one stream, each with its columns (`LockSpec.columns`: the packed LRV proxy, D71); the open checks then agree trivially. Updates D34, D40 | Today's per-stream pick tears the seam unseen, and an all-or-none present on per-stream upload freezes it when a lens lags; the group base makes a common time always exist. At 8K every frame is a reference frame, so a slow lens can't catch up and the pair must pace to it. Two opens already read nothing twice | M3.8, M3.14 |
| **D68** | **Schema 2**: `Media.source` (equirect, dual fisheye, EAC pair) with `#[serde(default)]`; `caps`; the calibration snapshot, raw and parsed, with its hash; geometry, the tune, and the gains beside it (Auto's track stored; `Gains` and `AnalysisInput` are core's, since sphere cannot name core); every hash as 16 hex digits from core's FNV-1a; `set_stitch` replaces the tune, the gains or both; no proxy field; one schema for all of M3; `set_source`, `set_stitch`, `set_caps`. Updates D47, D48, D57 | Every render reads only the project, so the CLI and agents match the window with no trailer reader and D57 holds. One migration instead of three | M3.9 |
| **D69** | **Export from the pair**: two decoders on K1's path (K4's on huginn); a held pair whose times must match; the end at the shorter lens; memory held to K3's one-lens baseline, with its levers in order; D56's viewer-first pace counted in pairs (`pairs_ahead`); the master only; 5.7K30 and 8K30 both gated over the whole export (8K30 on all of clip 8); M3.11b if K1 fails, or for huginn if K4 or K2's export half does. Updates D52, D56 | Parity with the viewer and PROMPT.md:77's "always export from originals". Decode is the measured bottleneck, so it is measured first. One lens can fill a per-slot lead alone, so the lead counts pairs | M3.1, M3.11 |
| **D70** | **The viewer and the Stitch pane**: stitched by default with Raw lenses one toggle away, and shown as lenses, with a notice, when the `Known` entry has `stitchable: false`; "Stitch it" and "Treat as equirect" as one-click `set_source` edits; the stitched overview with its band; geometry read-only with Swap lenses, the conventions behind Advanced; per-lens numbers, seam width and distance; Alt-drag; Measure seam and Estimate distance as jobs, Estimate distance standing aside while the viewer plays (D56); the dropped-project fix. Updates D40, D54, D55 | Tuning by eye alone is guesswork, and the conventions are for take 14 to settle, not Pat. The same number the tests use, with no wait on the UI thread. PROMPT.md:47 asks to tag a file as equirect | M3.0, M3.10, M3.13 |
| **D71** | **LRV proxies**: paired by name and record 4's bytes; opened as a lock group of two members over the LRV's one stream (`open_locked(lrv, [0, 0], LockSpec { columns })`), each member decoding the stream and keeping its half through `render::upload::Columns`, one slot per half, shown all or none by `pick_pair`; D25's paths as for any file (path B where the device is shared, through `Columns::buffer_planes`; the copy path elsewhere; downloaded under VA-API's opt-in import); time by pts; a per-viewer setting (Off or WhenPresent); not in the project; never for export or agents. Updates D31, D40; builds on D67 | The LRV is its master scaled by exactly 1.000. Separate slots avoid D31's bleed. Slots, uploads and the presenter go by stream and time, so two halves of one frame show together only through the lock; a second 1664×832 decode costs little (at worst 15 to 17× real time on odin) and reuses the lock whole, where a part index would change every (stream, time) match in playback. Path B is D25's default, Pat's choice, and the crop costs it one check. LRV counts differ at 59.94 and 119.88 | M3.14 |
| **D72** | **Exposure and white balance**: the overlap estimator with a symmetric split; Off, Fixed or Auto; Auto as one zero-phase track per clip stored in the project with its basis, the zero phase a forward-backward one-pole with τ = 1.2 s at 4 Hz; record 4 marks restarts; the default from P-GAIN; the analysis (on the master a full decode on the export's download path, non-reference frames skipped) and Estimate distance stand aside while the viewer plays (`Pace::stand_aside` before opening decoders and before each frame; Cancel still works); the automatic rerun waits for play to stop; an agent's Auto runs as a D58 job, never inside `edit`. Updates D56, D58 | The X5 records no per-lens values. One stored track gives every renderer the same numbers. D56 found that a second decoder on the viewer's engine makes it drop frames, and Pat chose that such work pauses while the viewer plays; this extends Pat's choice to the app's other decoders and does not reopen it. Taking every 6th 8K frame saves no decode, since every frame is a reference frame | M3.13, M3.15 |
| **D73** | **Stitch profiles**: XDG config keyed by vendor, model and serial; versioned JSON; applied by copy with provenance; a per-camera default; `core::dirs`; agents beneath a read root. Updates D45 | Calibration is per unit [F]. eframe's storage is invisible to the CLI and agents | M3.16 |
| **D74** | **GoPro MAX EAC**: `fs_eac`; `camera::gopro` reads `PRJT` and `PMOD` from the `udta` GPMF header blob only, with the (W, H) table as fallback; max2sphere out of tree as a reference; the `gpmd` track stays M4's; camera converts gopro.md's left-handed table to D33 by negating z, asserting det[r d f] = +1; `synth::eac` is written from D33 and FFmpeg `v360`'s YouTube EAC layout, never from gopro.md's table | gopro.md derives the layout from `PMOD` (§2.2), and its MAX 3K row doesn't fit the table. A rotation would mirror every face, and a writer built from the same table would not see it | M3.18 |
| **D75** | **DJI Osmo 360**: raw from dji.md §10's finding with a `Kb` lens on the equidistant base, OpenOSV's field numbers, `known` starting on `native_refine` (slots 1 and 2) and provisional conventions, or the written finding and the stitched-export path, tagged as equirect through `set_source` ("Treat as equirect" in the app); no stitcher constants, DJI's default `far_11` among them | PLAN's criterion; dji.md §11.2 | M3.19 |
| **D76** | **Nadir and zenith**: a polar equidistant cap in rig space, fill or patch; the cap's orientation as 18.3 defines it (forward at the nadir PNG's top and at the zenith PNG's bottom, +X right, `rotation_deg` > 0 clockwise as seen), tested with a glyph and a mirrored control; `fs_project_caps` for equirect; `set_caps`; a world anchor waits for M4 | nadir-patcher's radial mapping; its orientation flips both ways, so ours follows sphere's `Down` and `Up` faces; a cap that follows the tripod, turned by M4 with the picture | M3.20 |
| D9, D15, D22, D25, D28, D31, D32, D34, D40, D45, D47, D48, D52, D54, D55, D56, D57, D58 | Updated in the slice that changes each: the stitched viewer and Raw lenses (D54) and the stitched overview with its band (D55), by D70 in M3.10; Tier 2c, gated on `testkit::tier2::require_samples_cpu` with only `SPHERECUT_SAMPLES` (D9); camera's limits (D15); crate rows (D22); zero copy with pairs (each lens imported or copied out by itself and drawn through `StillRenderer`'s per-lens `PlaneSource`, 10.4) and with the proxy's halves (path B through `Columns::buffer_planes`, no exception), and huginn's path by D25's own revisit rule: K2 makes path A the default where Vulkan Video can't decode if huginn's numbers favour it over the copy and `zero_copy_matches_copy_path` passes there (D25); layout (D28); entry points, group 3, `Columns` (the crop of a packed proxy's half on the copy path and path B) and `upload_from_buffer`'s end check at wgpu's rule (D31); `dfish_*` rows, weighted metrics, `must_fail`, the manifest (D32); the lock (D34); overlay, raw view, proxy setting (D40); profiles under a read root (D45); schema 2 (D47, D48); pair decode (D52); the viewer-first pace counted in pairs for the export and extended to the exposure analysis and Estimate distance (D56); the three ops' examples and the reference project's camera media 11 (D57); an agent's Auto analysis as a job (D58) | | as listed |

## 24. Questions for Pat

1. **The new X5 takes** (SAMPLES.md 14 to 17, all in-camera, copied untouched with the `.lrv`):

   | Take | Exactly | Settles | Without it |
   |---|---|---|---|
   | 14 | 8K30 on the tripod outdoors, everything at least 10 m away, level: hold 15 s, then turn the camera about 90° on the tripod, level it again and hold 15 s more. Please note the height from the ground to the lenses, and that nothing stood nearer than 10 m. Long horizontal lines (horizon, roof edges) **and** vertical ones (trees, poles, building corners) at both of the camera's sides, where the seams run | the focal scale (Q4), V3 or V6, V6's terms, the Euler class finally, using the second heading as the second scene the fine class needs (8.4) and the height for the ground model, the real-footage noise floor that every seam gate needs | the scale stays C, a guess worth 41 px at the seam if wrong; seam gates stay "recorded"; without the height, take 14 fails the usable-take rule (8.6), and we would ask you for it, or for a re-take |
   | 15 | 8K30, 10 s: cover the screen-side lens with a hand for the first 2 s, then take it away | which track is the screen-side lens (Q2a), so the default view faces the right way and the lenses are named front and back | lenses stay "A" and "B", and the default view may face backwards |
   | 16 | 5.7K at 29.97 (or the nearest 5.7K mode), 20 s, anything | the 5.7K30 real-time target on a real file | gated on the synthetic 2×2880² clip only |
   | 17 | 8K30 handheld, 30 s, from deep shade into sun and back, one lens facing the sun | the exposure gate and the matching default | the gate stays "recorded" on clip 9 |

   Horizontal lines measure the seam's roll and the binding; vertical lines measure the scale,
   which is the error clip 8 can't see. If take 14 fails the usable-take rule (8.6), the request
   for a re-take says what was missing. Could you also copy the four takes to the cloud samples
   folder (about 1.6 GB with their LRVs, from clip 8's 17 MB/s [C]), so the fit runs where the agents
   work? Optional: one 8K30 take after any firmware update (a table row, and whether record 12
   appears); and, if the X5 can lock exposure and white balance, a second take 17 with them
   locked, which tells a per-lens auto (Auto needed) from a fixed sensor difference (Fixed is
   enough). Nothing waits for these takes: without 14 and 15, M3 closes as "done pending
   samples", the X5's geometry stays on the provisional numbers from clips 8 to 10, and the fit
   runs on the takes whenever they come (M3.12).
2. **Vendor pixels, for checks only.** (a) A Studio stitched export of take 14 (clip 12),
   compared against our stitch if the fit leaves a question open, never fitted to. (b) The
   1280×640 thumbnail the camera writes into each file (record 2), used to check which way is up
   and forward. Neither is vendor code; both are the vendor's stitch. Yes or no to each? Without
   them, the fit and the horizon rule stand alone.
3. **Defaults that change what you see** (recommendation first):
   - the lens at the centre of the default view: the one away from the screen, once take 15 says
     which track that is;
   - stitch distance 2 m: from 1 m to ∞ it keeps the parallax at the seam under 0.90°, the best
     single value; ∞ is right only for far scenes;
   - seam width 10° (a feather of ±5°);
   - proxies WhenPresent, since PROMPT.md:77 asks for the `.lrv` "instantly when present"; Off
     plays the master unless you toggle.
4. **GoPro MAX or MAX 2, and DJI Osmo 360**: which do you own? Clips 1, 3, 6, 8, 9 and 12 of the
   short set when you can.

**For your information, no answer needed:**

- **huginn.** Two prompts will come for you to paste. M3.1's is one paste: the H1, H2 and 4K30
  rows still pending from hw-run-6, the pair-decode benches and path A's check, with about two
  hours of making perf clips first at odin's pace. M3.17's is the full pass. Nothing waits for
  either; huginn's rows read "pending huginn" until they come back.
- **telemetry-parser.** PLAN's "via telemetry-parser where it fits" (PLAN.md:234) and PROMPT.md:52's
  "starting with Gyroflow's telemetry-parser" are read as not fitting in M3. We read the X5's
  trailer and DJI's calibration ourselves (D62), because its geometry is Studio-derived and
  crates.io's 0.2.x predates the X5. Whether M4 uses it to parse the IMU stays open.
- **The exposure analysis and Estimate distance pause while the viewer plays**, as an export does
  since your D56 choice: they start once play stops, the Stitch pane says "Waiting while the viewer
  plays", and Cancel still works. On a clip without its LRV the analysis decodes the whole 8K
  master, about 33 s for clip 9 on odin's Vulkan Video [C].

## Appendix A. Not in M3

Each is listed in PROGRESS as unavailable, not stubbed.

- **Better seams:** optical-flow seam refinement (M9), a stitch distance per azimuth, multi-band
  blending, keyframed stitch distance, a body or selfie-stick mask.
- **Motion:** stabilisation, gyro sync and the IMU's axes (M4; record 3's extent is read, never
  interpreted); rolling-shutter correction (M9); a world-anchored nadir cap (M4).
- **Other Insta360 bodies:** the X4, X3, ONE X2, ONE RS and X6. Their strings parse, but each needs
  a `Known` row from its own samples; two-file `_00_`/`_10_` cameras pair by name, untested.
- **X5 modes without samples:** I-Log and 10-bit, 360 audio, split recordings (they open as
  separate clips; joining is M5's).
- **Generated proxies** for cameras without an LRV (M5); GoPro's LRV as a proxy.
- **Undecoded records:** 9, 11, 22, 28 and 29 are listed with their sizes only.
- **Stereo** dual-lens sources.

## Appendix B. Where the drafts or judges disagreed

| Question | Decided | Why |
|---|---|---|
| An override `SOURCE` or an entry point per source | entry points | wgpu-core 30.0.1 counts every referenced global before overrides exist (10.1) [F] |
| The fit in `camera` or in `render` | `render::seam` | one instrument, at opt-level 2, beside the reference it samples through |
| telemetry-parser as an oracle (asked by two drafts) | not used in M3, not even as an oracle; decided, not asked; M4's IMU choice stays open | 5.5's checks cover the walk; git sources are forbidden; its geometry is Studio-derived; OpenOSV's proto gives DJI's field numbers (20) |
| The proxy in the project | not stored | export and agents never use it, and the player's open job finds it from its name and two trailers |
| The exposure track in an XDG cache, or a schema 3 | stored in schema 2 | one migration; the project renders alone everywhere |
| The conventions as Pat's switches | behind Advanced; Pat gets Swap lenses | they are take 14's to settle |
| 48 Euler candidates, or a direction field | 1152 values, no direction field | 48 favours known readings; a direction field duplicates every value (6.2) |
| Errors in one draft each: the binding decided first by NCC; Q5 on the second harmonic; the exposure split's signs; a 7-term rig lens | fixed: a joint search (8.4); the first harmonic (8.2) [C]; g0 = exp(+L/2) (16); 13 terms (10.3) | a swap can be a common rotation; Brown's terms give a first-harmonic field at the seam; the other signs double the mismatch; every V6 reading must render as the fit sees it |
| Seam width 5° or 10° | 10°, Pat confirms | the X5's overlap is ±10° and a 10° band still leaves the mask margin |
| Proxies off or on by default | WhenPresent recommended, Pat confirms | PROMPT.md:77 |
| A 5.7K30-only export gate | 5.7K30 and 8K30 | PROMPT.md:134's 4K30 export target names no source, and most of Pat's takes are 8K30; the row isn't narrowed silently |
| No GPMF at all for GoPro | the `udta` header blob only | gopro.md §2.2's rule and its MAX 3K row |
| A per-PR evidence ledger | the slice note | the same rows in one place |
| 22 slices (judged many) | kept | each has a done-when one reviewer can check; the end-to-end point is still the sixth slice of its chain |
| The pair-decode bench in M3.0 | its own slice, M3.1, on the export's path | playback's pair decode is already measured on odin (section 2) |
| `to_calibration` before sphere's types | M3.5 | M3.2 starts on day one |

## Appendix C. The fresh review's findings and where they were fixed

Five reviewers (code accuracy, geometry and maths, verification, scope and constraints, media and
performance) read this plan against the code on 2026-11-01, and a skeptic checked each finding,
by the code or by computation. All 58 stood. Seven groups then fixed sections 2 to 21 and one
fixed sections 1 and 22 to 24; their scripts are kept with the session's notes, not in the repo.

| Finding | Severity | What changed, and where |
|---|---|---|
| CODE-1 | major | Confirmed on the code (upload_placed, shapes and write_plane read from the top-left, shown_regions is path B only, slots and pick go by stream and time). Section 15 now opens the LRV as a two-member lock group over its one stream, so pick_pair shows both halves all or none. Each half is cropped through a new `render::upload::Columns` view, which upload_placed, StillRenderer and the reference take unchanged and which path B takes through shifted buffer planes. Section 2 now says where the top-left crop lives, 4.2 rung 6 and 10.5's LRV row name the path, and 19.3 lists the new property. D71, D31's update and M3.14's cells are in slice_changes. M3.8 is already upstream of M3.14 through M3.10 (the skeptic's correction), so no new dependency. |
| CODE-2 | major | Section 9's Open bullet takes the timing through a new `MediaBackend::track_timing` (FfmpegBackend from P-TRK's MP4 tables, `None` otherwise, which refuses with field "timing"), read in `OpenJob::run` whatever the decoder preference plus each worker's `VideoStreamInfo`; 9's Tests name the `PerStream` backend that plants per-stream tables, counts, GOPs, start pts and costs; 4.3 P-DEC, P-TRK and 7.2 make the two-track perf clips MP4 so the reader and the lock open them. |
| CODE-3 | minor | 11.3 now gives the reference project an optional camera media 11 (a synthetic 2×384² `.insv` that D57's test writes and probes) named by the `set_source` (camera form), `set_stitch` and `set_caps` examples, has agent `edit()` expand the camera form server-side while `wired()` skips it, and adds the three ops to core's per-op samples; M3.9's row changes through slice_changes. |
| CODE-4 | major | Confirmed on the code (all five entry points call render_from with one frame and one private Source, still.rs:326-435, 690-697; agent::still never calls render, agent/src/still.rs:984-1000); 10.4 now routes every entry point through a render_from that takes SourceFrames with a per-lens LensFrame { frame, from: PlaneSource }, render_blurred and render_yuv take it, a new Callers bullet gives agent::still's pair path on path B, DMA-BUF and bytes and the export's, with a Tier 1 test and its control and a Tier 2h twin; section 14's frame row says how the pair reaches the GPU; M3.7, 22.3, D25 and M3.11b follow as slice changes. |
| CODE-5 | minor | Confirmed (core depends on sphere, core/Cargo.toml:15; Fnv64 is testkit's only, testkit/src/lib.rs:22-48; AnalysisInput undefined); took the review's second option: Gains stays in core as a new DualFisheye.gains field beside the tune, so 6.2's Tune and RigLens lose their gains, 11.1 defines Gains::at and AnalysisInput and makes every schema-2 hash a 16-hex-digit string from core's fnv1a64 made public (also 5.4's calibration hash), 10.4's pack and render_rig take the frame's gains, and 11.3's set_stitch says it replaces the tune, the gains or both; D68 and 22.3 follow as slice changes. |
| CODE-6 | minor | 4.4 now gates Tier 2c on a new `testkit::tier2::require_samples_cpu()` that needs only `SPHERECUT_SAMPLES` (the old gates call `require_hw_run`, testkit/src/lib.rs:69-80, 108-109) and gives the exact cloud command with `SPHERECUT_TIER2` unset; M3.0's row and D60 change through slice_changes. |
| CODE-7 | minor | Confirmed (layouts::view is binding 0 alone, layouts.rs:73-78; draw binds view_group at group 0, project.rs:677); took option (b): 10.2 gives each LinearTarget a second group-0 bind group rig_group from layouts::view_rig and moves the cap texture with a caps uniform into group 2 beside the sampler (layouts::sampler_caps, four groups owned by the media's caps plus Projector's caps-off defaults), so fs_project's layout and goldens stay as they are; 18.3's sentence is corrected to match, and D66 and M3.20 follow as slice changes. |
| CODE-8 | minor | Confirmed and fixed with option (a): 6.1 and 10.3 apply KbLens's base after the polynomial, as Lens::radius does, with the base carried per lens in prism.w (orthographic masked past 90°); dfish_kb_rig renders all four bases so its control fails through the shader, and 18.2 names the equidistant base for DJI. |
| CODE-9 | minor | All five confirmed and corrected: section 2 (fs_project reads the source at 183-190, 194-198, 203-205 and 207; add_media at agent/src/ops/edit.rs:271-285, also in 11.3; mux_video_streams under media::encode, encode.rs:1311), section 14 (equirect_doubt at agent/src/still.rs:181, through check_equirect 236-316) and 5.4 (camera keeps its pinned shape per docs/cli-json.md:49-53: the CLI's CameraGuess object gains serial, the agent's summary keeps its string and gains camera_detail); M3.2's done-when follows as a slice change. |
| MATH-1 | major | When the roll is applied first, a 180° roll offset is exactly a common 180° turn and invisible to the seam; when it is applied second or last it moves the relative rotation by 0.2° to 1.24°, as the plan said. With the roll about z applied first, (Y, 0), (X, ±90°) and (Y, 180°) are exact twins. Fixed in 6.2: a Twins paragraph replaces the old sentence, and `upright` becomes a quarter turn about −Z. Stage G (8.4) now keeps only values that put block 0's lens within 5° of −Z: 2304 of the 4608 remain, and each one dropped is an exact twin or not back to back. Q6's rule (8.6) compares only stage-G candidates more than 2° apart and leaves nearer ones, twins included, to stage J. The up-and-down rule picks one of four quarter turns, and its redirect keeps `upright` at 0 and records it as open. 3.2 U4, 3.3, 8.6's provisional paragraph and risk 1 are updated. A new property, `euler_twins_are_quarter_turns`, is added (slice change to M3.3; D64). |
| MATH-2 | major | With q free per 15° sector, the tilt's standard error is 12 to 17 times what it is with q held, and the sector q absorb 93 % of a 0.3° tilt, leaving 0.015° (0.3 px). Fixed in 8.4: stage F profiles the 24 sector q out (a 5×5 solve), returns its information matrix and reports any rotation direction whose σ exceeds the spacing of the competing predictions as undetermined. Stage J judges only the determined directions, on a fit with both centres held. Clips 8 and 10 now settle only the gross class and the relative roll; the fine class waits for take 14. Updated: 3.2 U4, 3.3, the 3.4 conclusion, 8.2's q note, 8.6's Q6 rows and provisional paragraph, 8.9's print, risk 1. |
| MATH-3 | major | With the stored centres held, the three V6 readings the plan named (now `V6Reading` values, 6.1) move a seam pixel by 12 to 33 px against `RadialOnly`, 12 to 32 px of it along the seam, and lie 20 to 36 px from each other. V3 against V6 is 8 to 13 px on lens 1 as well as 11 to 17 px on lens 0. Fixed: U6 and U7 get their costs in 3.2 and U6 its new settled-by. 3.4 gains two rows (V3 against V6 for lens 1, and the V6 reading). 8.2's Q5 paragraph is rewritten. 8.6's Q5 is now ranked on clips 8 and 10 on the along-seam terms, with a 'no listed reading fits' redirect, and 'not measurable' applies only below the floor. Risk 1 updated. No Brown–Conrady KpS variant is added: it has an r⁴ tangential term, which the canonical form lacks. |
| MATH-4 | minor | Confirmed, though some V6 points need 7 passes, not 6: 6.1's inverse now repeats the strip-and-solve pass until the forward residual is under 1e-12 (at most 20 passes, bracketed on [0, m(limit_theta)], None otherwise), Gyroflow stays only as attribution, 6.3's Unified bullet states its tolerances in the lens's own pixels, and D63 follows in slice_changes. |
| MATH-5 | major | 6.1 adds rig::theta_frame (99.1° to 100.7° on Pat's V6 numbers by lens and map [C]), 10.3 adds a uv-box frame term used by the shader, the CPU weights (6.2), lens_coverage, the strips and Measure seam, 11.2 bounds the feather by min(θ_max, θ_frame) − 90°, 6.3 adds a Mask property, and 7.1's X5-like truth carries content to 106° with a new lens-0-at-104° case and a frame-term-off control in dfish_mask_margin (10.3, 10.5). |
| MATH-6 | major | The binding reversed or the tracks swapped is a tilt (a1/b1) of up to 2.3° (at most 1.1° with the roll about the lens axis), plus 0.4° to 0.6° along the seam from the swapped centres. It is not a c0 roll, and it does not collapse NCC: the tilt is at most 2.4°, inside the ±3° search. A lens late under a turn is a rigid relative rotation ωΔt, on a1/b1 under a pan and on c0 under a turn about the lens axis; it is not on rest. Fixed in 3.2 U2, 3.3, 3.4's binding and lens-late rows, 8.2's table cells, 8.3's 'predicted term' rule, 4.2's controls and 4.3's sub-frame regression. 15's rung-6 sentence, 'c0 and NCC collapse', belongs to the LRV group and is left to the merger (notes). |
| MATH-7 | minor | r/(dr/dθ) = 90.36° at the seam, so 0.18° of residual parallax at 10 m is 0.10 % of scale, not 0.17 %, and it biases the fitted scale high, towards A. Fixed in 8.6's Q4 cell, which also gains a guard: the scale–tilt correlation must be under 0.9 in size, and the fitted tripod height must be within 10 % of Pat's. The 0.25 % threshold stays. |
| MATH-8 | minor | Distortion modelled wrong in θ is radially symmetric, so at θ ≈ 90° it is uniform across the seam and lands on a0, not on a2/b2. Equal offsets of both centres are a pure tilt (0.0375° per canvas px), so holding lens 0's centre is a gauge choice, not 'only the difference shows'. Fixed: in 8.2, distortion in θ moves from a2/b2 to a0, and the a1/b1 cell notes the centre-sum gauge. 8.4's stage F names the gauge, and stage J judges with both centres held at their stored values, since a held centre wrong by 1.3 canvas px would use up the whole 0.05°. |
| MATH-9 | minor | Partly confirmed and fixed in 6.3: the bound is now θ ≤ min(100°, limit_theta − 1°) with the fold included, and the generator scales the drawn coefficients with ξ (a degree-n term by (m_X5/m_top)^(n−1)) instead of discarding draws, so small ξ stays tested and every draw converges [C]. |
| MATH-10 | minor | 7.1 sizes the corner block at b = s/12 (32, 64 and 320 px) and has synth assert b ≤ s/2 − /centre offset/ − (r(θ_max) + 2)/√2, which is 47.8 px at 384² and 100 px at 768² for the X5-like truth [C]. |
| MATH-11 | minor | 16 now states the kernel: a forward-backward one-pole, τ = 1.2 s at 4 Hz, each pass starting from its segment's first sample. The ramp test now compares against the truth run through the same filter, within the constant case's 1 %, with a causal-filter control that misses by 16 %. |
| MATH-12 | minor | 18.1 states the z negation and adds a det[r d f] = +1 unit test, writes synth::eac from D33 and FFmpeg v360's YouTube EAC layout, and adds an eac_cells glyph with a 180°-turn control; 18.3 defines the cap's orientation from sphere's Down and Up faces with a glyph test and a mirrored control (10.5 cells; D74 and D76 in slice_changes). |
| VERIFY-1 | major | Ground 1.5 m below the rig, left unmodelled with 30° to 50° around the nadir excluded, puts +0.4° on a0 and −0.6° (12 px) on b1. With 81° excluded it is still 4.5 px. Fixed in 8.4: take 14 models the ground as a plane, q(φ) = max(0, −sin e)/h, with Pat's tripod height. The far field is held at q = 0, because a uniform q is degenerate with the scale. h is also freed as a check, and only 30° around the nadir is excluded. 8.6's usable-take rule now asks for a sector residual ≤ 0.18° after the ground model, both headings and Pat's height. The Q4 data cell and 3.3 are updated. 7.1 gives `RigTruth` a ground plane, and 8.5's R = ∞ case has the ground switched on and off. Slice changes add the height and the second heading to question 1 and M3.0. |
| VERIFY-2 | major | same root as MATH-2. Same fix: the information matrix, the undetermined directions and stage J on determined directions. Condition 4 now names the second scene by what stage F determines: clip 10 at rest or take 14 for clip 8, and take 14's second heading for take 14. In 8.6, Q2b on clips 8 and 10 now goes through the along-seam part of the swapped centres, and the X5 entry takes only the gross class from clips 8 to 10, with the fine class recorded as open. 19.4's M3.6 cell and 8.9 print the undetermined directions. |
| VERIFY-3 | major | A lens late by part of a frame under a turn is a rigid rotation, so it biases the fit and is invisible to a whole-frame k sweep. Clip 10's 'turned frames' are moving frames. Fixed: stage F uses only frames at rest (lens 0 turning under 0.02° per frame, measured from its own strip). 4.3 adds a per-frame regression of a1, b1 and c0 on lens 0's turn, whose slope is the offset in frames. Condition 4 uses frames at rest after a turn. 8.2's rest-rms cell and its separation note are updated. 7.1's `RigTruth` gains a per-lens offset and a rolling-shutter readout. `fit-matrix` gets a half-frame, 21 ms readout truth with an expected verdict. |
| VERIFY-4 | major | Every planted truth in the validation expected a 'win', so no branch for fitted, not measurable, indistinguishable or out of family was ever exercised. Fixed by rewriting 8.5. Tier 1 runs 2 geometries with expected verdicts as `must_fail` controls: a tilt-only neighbour that must stay open at R = 1.5 m, the ground switched off, f × 1.012, and a 0.5° yaw giving `Extrinsics::Fitted`. `fit-matrix` adds two headings for condition 4, a relative rotation off every prediction, a scale of 1.006, d5 to d12 = 0, a binding swap that is a common rotation, KB rendered and fitted as Mei, and lens late with rolling shutter. 8.6's Q5 redirect is made reachable. 19.6 and risk 1's controls cell are updated, and slice changes to M3.6 and D65. |
| VERIFY-5 | major | 4.1 rule 2 now covers the extrinsics, 7.1 adds synth::lens::compose with its own enums and hand-worked unit tests (8 EulerConv values, block 1 under A, B and C at 3840² and 768², c_1 under both tz signs), 7.2's Planted and RigTruth comments follow, 6.3 adds an Against synth property, and section 21's risk 2 names what rule 2 covers. |
| VERIFY-6 | major | Section 9 defines `unpaired_presents` over every stream the player shows, locked or not; the Tier 1 mixed test reads both presented slots back at every present and counts content mismatches apart from tags, with the lock-off control above 0 on both, and asserts landing within WAIT; liveness is the pacing test; Tier 2h reads the perf clip's corner codes from both slots on every frame through a region readback and plays clip 9 instead of clip 8; P-LOCK and 19.4 say so. |
| VERIFY-7 | major | Outside-minus-inside PSNR measures the scene, not the blend: with no seam at all, a 1° checker scores about 16 dB below the grid at 0.70°/px. This departs from the review's fix: its comparison against the weighted mix of the two lenses' errors can never fail, because a weighted mean's squared error is never above the mean of the squared errors. Fixed in 4.2: the band check compares the stitch with the better single lens alone, over the same pixels of the 256² seam views, within 1 dB. A new control, lens 1 turned 0.13° about +Y (0.5 px), fails it by about 3.6 dB in a 1-D model. |
| VERIFY-8 | major | Feather 0 and margin 0 cannot fail at rung 1: on matched lenses a hard switch makes no step, and with h = 5° a lens's weight is 0 beyond 95°, which mip bleed reaches only at λ ≥ 4. Both were removed from rung 1's controls (4.2). `dfish_feather` (10.5) now uses lens 1 at +0.25 EV with gains off (10.6 LSB): the stitch must change by under 1 LSB per pixel on the 256² views (about 0.6 expected), and its h = 0 control must show a step of at least 8 LSB. `dfish_mask_margin` moves to lens-alone views at λ ≥ 4. This departs from the review's fix in two ways: it drops the 0.3 px misregistration, which has no effect on a flat ramp and would be swamped by checker noise at a 1 LSB per pixel gate. and drops its θ_max = 95° option, which cannot fail because the weights there are under 0.05. |
| VERIFY-9 | major | 768² × 2 at ss = 2 is 4.7 M samples (the n × n loop at synth/src/lib.rs:336-339), not 2.4 M. Rung 1 with about 14 controls cannot fit in 4 s, nor 6 geometries at two depths in 3 s. Fixed: 7.1 restates the cost (about 2 s a pair) and adds a per-binary render cache, and 8.3's 'About 2 s' is replaced. 8.5's Tier 1 recovery drops to 2 geometries (about 15 s), and the 6 geometries move to `fit-matrix` (about 15 minutes [G]), which joins the push rule. 19.6 now reads rung 1 ≤ 15 s and recovery ≤ 15 s, with M3's total about 120 s. Slice changes to M3.4 and M3.6. |
| VERIFY-10 | minor | The plan's 50.8 and 53.2 dB are odin's measurements on the synthetic grid; the floors are 47.7 and 50.1 (`tier2_export.rs:223-244`). Gains off can't fail on real footage, where a 1 % lens ratio leaves the export near 49 to 52 dB, above the floors [C]. On clip 1, gains off is replaced by a planted Fixed 1.25/0.8 fault (about 25 dB [C]) on views that see both lenses (flat on a side seam). Both frame-late controls stay (consecutive frames differ by 31 to 40 dB [F]). Clip 1's numbers are recorded, and a floor under them changes only in a reviewed change. |
| VERIFY-11 | minor | Every rung-1 control is relative between the lenses, so a slip common to both passes every seam check, and a gate on the blessed value inherits any slip present at the first bless. Fixed in 4.2: rung 1 also needs PSNR at least 3 dB above a common-mode control's (both lenses turned 0.5° about +Y), which is D32's form. The common-mode control is in the controls list: it must pass the seam checks and fail the comparison with the ideal. A 1-D model gives about 6 dB; the 3 dB margin in 2-D is [G]. |
| VERIFY-12 | minor | Rungs 4 and 5 ran on a default-tune project whose reference read the same project, so a dropped tune passed both. Option (a) is taken: 11.5 gains a fixture (the rung-3 file with q = 1/1.5, lens B yawed 0.3°, a 3° feather and Fixed gains 0.9, plus an Auto twin), and the test builds the reference `Rig` itself through `Rig::build`. A tune-dropped control is added in the export (rung 4) and in the viewer (rung 5); dropping the tune moves lens B 1 to 5 px and mid-grey 6 LSB [C]. |
| VERIFY-13 | minor | LRV part only. Section 15's `lrv_matches_master` now records each pair's median SSIM and σ (per-frame SSIM over 20 frames spread across the LRV's 30-frame GOPs), and a reviewed change then gates it at the lowest median less 3 times the largest σ, with both controls below the gate. 19.4's cell is corrected and M3.14's done-when is in slice_changes. σ in 8.4, P-TRK's σ and the seam rows are the geometry group's. Confirmed for the σ parts. σ over 10 consecutive frames ignores the correlation within a GOP and the information matrix. Fixed in 8.4: the noise floor is the larger of the spread over 5 disjoint frame sets spread across GOPs (`--frames 0:300:30`) and the information matrix's standard error, and never under 0.05 px. The spread between clips 8, 10 and 14 is reported beside it. In 4.3, P-TRK's σ is taken over disjoint 1 s windows. In 19.4, the M3.6 and M3.12 gate cells use 8.4's σ, with a slice change to M3.12. The SSIM ≥ 0.95 part (`lrv_matches_master`) belongs to the LRV group. |
| SCOPE-1 | major | 4.1 rule 7 now bans video-types' statements of vendor internals, 6.1 replaces the four named V6 readings with a generic 21-value V6Reading space (radial fixed at d0..d4, with GyroView/raw360-formats and orbis as examples), 6.2, 6.3 and 5.4 follow, section 20 credits raw360-formats, GyroView and orbis-reframe as facts only, and 22.3 fixes V6Reading in M3.3's first commit. |
| SCOPE-2 | minor | Fixed as the vfix proposes. M3.21 now depends on "all but M3.12", and its Done when lists the X5 geometry in PLAN and PROGRESS as waiting on takes 14 and 15. M3.12 gets a pending state: PROGRESS lists it as waiting, the X5 entry keeps M3.6's provisional numbers, and the `seam_quality_*` rows stay recorded. M3.12 also gets a short review of its own. M3.17's takes 16 and 17 are optional, with question 1's fallbacks written into its Done when. Section 1's bullet, the 22.2 diagram and text, and question 1 all say M3 closes as "done pending samples" without the takes. |
| SCOPE-3 | minor | Fixed as the vfix proposes, keeping the code dependencies. M3.6 always writes an entry: a new paragraph before 8.7, its Done when, and the `verdict` function in 22.3. A gross rule applies on clips 8 and 10 (8.6's Gross row and redirect cell). If it fails, the placeholder stays with `Known.stitchable: false` [G mechanism], and the app shows such files as lenses with a notice (5.3, 13.2, M3.10 (a), D64, D70). What Pat sees at M3.7, M3.10 and M3.11, and M3.11's note, waits for M3.6: this is in those rows, in 22.2 as a dashed edge with its text, and in section 1. Code that merges first says "placeholder geometry" in its note. |
| SCOPE-4 | major | M3.1's done-when needed huginn's K2, against 4.4's "Nothing blocks on huginn". M3.1 now merges on odin's K1 and K3. huginn's rows read "pending huginn" in 4.3, 4.4, 12, 19.4, 19.5 and risk 5 until its answer lands, and M3.17 takes them up. Slice changes rewrite M3.1's, M3.11's, M3.11b's and M3.17's done-when, and add M3.1 to M3.8's and M3.10's Depends on, since their odin gates read M3.1's perf clips. |
| SCOPE-5 | major | 7.2 and 4.3 P-DEC give M3.1 the two-track MP4 perf clips and the 2x64 test files through a new MP4 output of `mux_video_streams`, 9's planted faults come from `PerStream` instead of M3.5's InsvFaults, and slice_changes make M3.8 depend on M3.0 and M3.1 and correct 22.2's text and diagram. |
| SCOPE-6 | major | on odin Auto already picks Vulkan Video for H.264 and the shared device (decoder.rs:303-309, worker.rs:281-296), so "proxies use the copy path" broke D25. Section 15 now follows D25. Path B fills each half from the Copier's whole-picture copy through `Columns::buffer_planes`, and one end check in upload_from_buffer moves to wgpu's rule (computed: 832 bytes over today). The copy path is used elsewhere, and VA-API's opt-in import downloads. Added: a Tier 1 packed case in shared_vulkan.rs, Tier 2h `zero_copy_lrv_matches_copy`, and the drop bench on path B (D42), all in 15 and 19.4. No exception, so nothing goes to Pat. |
| SCOPE-7 | major | 13.4, 13.5 and 16 now put the exposure analysis and Estimate distance under `Pace::stand_aside` (nothing starts while the viewer plays, the frame in flight finishes, Cancel works) and make the automatic rerun wait for play to stop. Measure seam is noted as unaffected. Two kittests and a gui_smoke run with an Auto analysis pending are added; D72, D70 and the M3.13, M3.15 and M3.17 rows change through slice_changes. |
| SCOPE-8 | minor | Fixed. M3.0 (c) gains `require_samples_cpu` and is M3.0's first PR, merged on day one. M3.1, M3.2 and M3.3 depend on it. M3.8 depends on M3.0 (`track_timing`) and M3.1 (the MP4 mux, corner codes, perf clips and test files), and M3.10 also needs M3.1's 2×2880² clip. M3.7 "merges after M3.5". M3.18 gains M3.10 and M3.11, M3.20 gains M3.11, and M3.19 keeps its dependencies, with Pat sees noting the app. "Treat as equirect" is now owned by M3.9's `set_source` for agents and M3.10's one-click action, following PROMPT.md:47; 18.2, M3.9, M3.10, M3.19, D70 and D75 changed with it. The 22.2 diagram is redrawn to match. |
| SCOPE-9 | minor | Q4 asked Pat to approve what D25's revisit rule already settles, and Q3's exposure bullet and Q6 asked nothing of Pat. K2 now applies D25's rule as Pat wrote it and tells Pat in the slice note. Misses go to a decision card with options (section 12, risk 5, 19.5), and P-GAIN and section 16 point to M3.15's note. The slice changes drop Q4, move Q3's exposure bullet to M3.15's note, and make Q6 an FYI. |
| SCOPE-10 | minor | Partly confirmed and fixed as the skeptic proposed: section 20 limits telemetry-parser's non-use to M3 and names OpenOSV's proto as 18.2's field source; rule 7 adds telemetry-parser's DJI rig.rs, rig_mask_data.rs and the far_11 slot; 18.2 starts known on native_refine; D62, the M3.0 row, Appendix B and a section 24 FYI follow in slice_changes. |
| SCOPE-11 | minor | In 5.1, every camera reader (`read_trailer`, `read_record`, `verify_companion`, and profile list, load and save) now calls `may_block` and runs only from the OpenJob, the making job or the profile jobs; `companions` works on names only and `verify_companion` takes readers. 11.1, 13.5, 15 and 17 each say the same in a clause. `never_blocks` gains the `.insv` with its LRV, the proxy toggle and a control. The finding's detail that verify_companion takes paths was wrong; it took Trailers plus '…'. |
| SCOPE-12 | minor | PLAN.md:595 now says "the network access Pat's rule forbids", fixed in PLAN.md with this plan's commit. |
| MEDIA-1 | blocker | Section 9 gains the group base (the cache derives Tg, the largest common time at or before the presenter's, and runs its current-frame, window, eviction and back-pressure rules and the upload's wanted lists from it), a group `until` that never passes what the other members hold, the measured fact that 8K catch-up skips no decoding, and a `pair_resync` backstop; 9's Tests gain `a_locked_pair_paces_to_its_slowest_lens` (one slow lens, both slow, a halved budget) with a control that freezes; 4.3 P-LOCK adds 8K25 on path B, the pacing rule and huginn's rows; 19.4 and risk 4 in 21 follow. |
| MEDIA-2 | major | Section 9's Slots bullet asks for MAX_SLOTS whatever the setting and sets a floor of 2 x members live slots, below which the group unlocks with `LockRefused {field: "slots"}`, the raw view and a GPU-memory notice, with the as-it-arrives branch back; 9's Tests add the slots-killed-to-3 test (mid-play and mid-seek) with a no-floor control that must time out. |
| MEDIA-3 | major | Confirmed (SlotFrame has no part, `have(i, f.time)` would skip the second half, pick shows one slot per stream). Section 15 picks one design: two player streams over one file stream under section 9's lock, with no part index, so slot keys stay (stream, time). It adds `a_packed_proxy_never_shows_its_halves_apart` with a lock-off control that must tear, and rung 6 (4.2 and 15) now runs through the app's viewer, the slot ring and pick_pair. |
| MEDIA-4 | major | 7.2 now states the perf clips' container (one MP4 through `mux_video_streams`' new MP4 output, time bases kept), their coding per mode as measured on the 7 masters (3840 at 29.97: IDR every 60, P only, 65 Mbps; 2880 at 59.94: IDR every 120 with non-reference alternate frames, 80 Mbps; 2880 at 29.97 a guess), noise and the too-easy flag, and that from M3.10 synth's trailer is appended so the stitched and export benches open them as .insv; 4.3 P-DEC and 19.4's stitched row follow. |
| MEDIA-5 | major | huginn's 8K30 export had no gate and no usable redirect: K2 covers 5.7K30 only, and Redirect 1 (Vulkan Video) doesn't exist there (`docs/perf.md:241-244`). Added K4 (two VA-API download decoders at ≥ 36 fps a lens by K1's rule), whose redirect is an M3.11b path A row on huginn and then a decision card. K2 gains the engine row it lacked, since `bench_playback_x5_raw` plays samples only. huginn's prompt becomes one paste with the pending hw-run-6 rows and the perf clips' making time. Section 12, 19.4, 19.5 and risk 5 carry K4. |
| MEDIA-6 | minor | K1's "41 fps alone, near the line" misreads perf.md: 41 was one of three unstable runs (20 to 43), and the threaded pair, which `decode_rate` already measures, has come out at 22 to 31 fps a lens on odin against Vulkan Video's 58 to 61 [C]. K1 is rewritten from those numbers and expects VA-API to fail and Redirect 1 to fire. Its gated rows follow a fixed rule (600 frames, timed from first frames, beside a VA-API encode, median of three). Section 2's 41 and risk 5 are corrected. |
| MEDIA-7 | minor | Section 9's Fallback bullet keeps the healthy lens on hardware and moves only the failing one through media's wrapper, with the reason corrected (bit-exact decoders, 0.42x for two software decoders, a 3.7 s software exact seek); the FaultBackend test asserts lens 0 stays on hardware, pairs keep coming under the pacing rule, and one notice. |
| MEDIA-8 | minor | Count is no longer a refusing field (9 Open); `stts` and `stss` compare over the common prefix (TrackTiming keeps the tables, 4.3 P-TRK); the pair ends at the shorter lens with one `LensShort` notice and the planted fault asserts it; P-TRK's kill is now `stts` differing over the common prefix; the export half goes to M3.11's done-when through slice_changes. |
| MEDIA-9 | minor | The 88 MB counts only the channel; the export keeps 10 lens frames alive at 8K (221 MB [C]) before FFmpeg's frames, and "≤ 1.5 GB" had no baseline. K3 now holds frames as the export will and measures each run's peak growth in its own child process (VmHWM reset). It passes at twice the one-lens run plus 10 % [G], with its levers in order. This departs from the review's "+221 MB": the one-lens baseline already holds half of those frames, so adding 221 MB would count them twice. |
| MEDIA-10 | minor | Clip 1 has 114 frames, and `bench_export` counts frames from job start (`tier2_export.rs:448`, `job.rs:291-315`), so half a second of start-up reads a 36 fps export as 31 fps [C]. The 8K30 gate now runs on all 966 frames of clip 8, and 5.7K30 on all 600 of the synthetic clip and then take 16's 20 s. Clip 1 stays for parity only. |
| MEDIA-11 | minor | 16 now costs the master path as a full decode (every 8K30 and 8K25 frame is TRAIL_R, checked on clips 1, 2 and 8; clip 9 takes about 33 s on Vulkan Video and 151 s in software). It skips TSA_N at 59.94 and 119.88 with AVDISCARD_NONREF and decodes on the export's download path. Keyframes only are allowed only under a P-GAIN condition, and an agent's Auto runs as a D58 job, never inside `edit`. |
| MEDIA-12 | minor | `lead()` counts Ready slots of any stream (`crates/app/src/export.rs:393-395`), so for a locked pair the fast lens can fill the lead while the lagging one has nothing ahead. D56's viewer-first rule stays but is counted in pairs (`pairs_ahead`, a pure sibling of `pick_pair`): one pair at 6 slots, two at 8. The new app test `the_export_yields_while_a_locked_pairs_lens_lags` has a per-slot control that must fail. |
