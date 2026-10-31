# Spherecut M2 (Clip reframer): the implementation plan

*2026-10-30, before M2's code, at main be982e9. Written from PLAN.md M2
and A2, docs/PROMPT.md, DECISIONS.md D1 to D46, the code as M1 and A1
left it, and two research notes (`m2-research/encode.md`,
`m2-research/keyframes.md` in the session scratchpad, summarised where
they are used). A fresh review's findings become amendments in section
0, which win over the sections below.*


## 0. Amendments (read first)

**Amendment 1 (2026-10-31, slice M2.1's code).** What the model and
its edits became, where they differ from sections 3 to 5 (D47 and D48
record them):

- **Markers are on the clip, in the source's time** (`Clip.markers`,
  `add_marker {clip, marker}`), like keys, so trimming the in point
  doesn't shift them off their moment. Sequence markers wait for M5.
- **A flat output is `{aspect, short_side}`**: 1080, 1440 or 2160 on the
  short side for every aspect (1080 × 1350 for 4:5), instead of a
  `height` that meant width for portrait.
- **The view's names**: a key track is `KeyTrack<V>` (`Track` is the
  timeline's); an orientation key's value is `Orient::View(Orientation)`
  or `Orient::Aim {target, roll_deg}` (`Free` was easy to confuse with
  `OrientMode::Free`); the mode is `ViewAnim.mode`. FOV keys, aim roll
  and target paths are in degrees in the model and the file.
- **FOV keys are 10° to 360° whatever the projection**; the renderer
  caps each projection at its own maximum, as the viewer does, so a
  projection key never makes a FOV key invalid.
- **The readable copies are checked, not ignored** (section 5.1): a time
  whose `seconds` disagrees with `exact`, or an orientation whose
  `ypr_deg` disagrees with `quat`, is refused with where it is and
  which to edit; `seconds` may be left out and `ypr_deg` given alone.
- **Edits**: `set_key`, `remove_key`, `set_marker` and `remove_marker`
  name only the key or marker (ids are project-wide); `set_keys`
  replaces a whole track (the inverse of `set_view_path`, and the
  Inspector's "keep the vertical field of view"); `set_view_path`'s
  points carry the ids their keys were given, so redo makes the same
  ids; `rename {id, name}` takes a sequence, marker or target. A new
  thing's id in an edit is 0 or left out (`id_given` otherwise); undo
  and redo restore ids. Each edit is validated before the next, so the
  error names the edit that broke a rule. An empty list is refused.
- **`MediaVideo`** is the stream index, size, rate and duration, with no
  projection field; the fingerprint is the file's size and the hash of
  its first MiB. `Media::from_probe` (in media) moves to M2.4, where the
  export's tests first open real files, and core's `media_status`
  checks the size and hash on load.


## 1. Summary

M2 is the first version Pat can use for real: open one 360 clip, trim
it, aim a virtual camera with keyframes, and export a flat video for a
phone or a screen, or a trimmed 360 video that YouTube reads as 360.

What Pat does in the GUI:

1. Opens a clip (as in M1). The app makes an untitled project holding
   one sequence with that one clip.
2. Sets in and out (`I`, `O`, or the trim handles), drops markers (`M`).
3. Aims the view as in M1 (drag, scroll, the projection switch) and
   presses `K` to key it at the playhead: orientation, field of view and
   projection together. Plays it back and sees the camera follow the
   keys, eased.
4. Fixes a curve in the curve editor (yaw, pitch and roll shown from the
   orientation keys; FOV; the projection morph), sets easing per key,
   or points the camera at a look-at target that it keeps facing.
5. Sees on the sphere overview where the camera points and where it
   will go.
6. Undoes and redoes anything (`Ctrl+Z`, `Ctrl+Shift+Z`), saves the
   project (`Ctrl+S`), and reopens it later.
7. Exports: flat 16:9, 9:16, 1:1 or 4:5 at 1080p, 1440p or 4K, or 360
   equirect up to 7680×3840 with its metadata, in H.264, HEVC or AV1,
   on the GPU's encoder where it has one; the export runs in the
   background with progress and cancel, and the UI stays smooth.

Headless: `spherecut render project.spherecut -o out.mp4` does the same
export from a shell, with `--json` progress for scripts and A2's agents.

Underneath, and what A2 builds on: every change is an `Edit` value
applied through `Project::apply`, all or none, with ids that are never
reused, a revision number and labelled undo steps.

The milestone ends with the toolkit check Pat asked for (D26): a
timeline prototype of about 500 clips on 6 tracks that must hold display
refresh on odin and huginn at fractional scale, or a written-up problem
and Pat's decision before any switch. The prototype is also built and
run early, before the GUI work, so a failure is known in time (section
15).


## 2. What M2 builds on

- **core**: `Rational`, `Time` (`{exact, seconds}` in JSON), `FrameRate`,
  `TimeSpec` (the `--time` grammar), `StopSource`/`StopToken`,
  `fs::TempFile` (`persist_new`, `persist_replace`, D16), `may_block`,
  the submit gate (D41).
- **sphere**: the view convention (D33: `world = q · view`, +X right, +Y
  up, −Z forward; `Ypr` with `q = R_y(−yaw)·R_x(pitch)·R_z(−roll)`),
  `perspective::{Projection, Lens}` with the θ(ρ) morph, `grab` (level
  and free drags, `zoom_about`), `slerp`, `rotation_angle`, `LonLat`.
- **render**: `Gpu`, the upload paths (copy, belt, DMA-BUF and the shared
  Vulkan device, D31, D25), `Projector` with `ViewParams` and
  `ViewUniform`, `LinearTarget` (`Rgba16Float`, linear light), the encode
  pass (sRGB OETF into `Rgba8Unorm`), banded readback past 256 MiB,
  `StillRenderer`, the CPU reference, the goldens harness (D32).
- **media**: decode and frame-exact seek in software, VA-API and Vulkan
  (D29, D30), the hardware device per kind (`hw.rs`), audio decode and
  resampling, probe, doctor, `Mp4Writer` (libavcodec encode and the MP4
  muxer, written through `TempFile`), `inject` (st3d, sv3d and SA3D
  written into a finished MP4 by our own box writer, then re-parsed),
  `spherical` (read and write those boxes), the JPEG encoder A1 added.
- **playback**: the engine (decode worker, upload thread, presenter,
  frame cache, slots, audio thread, D34, D35).
- **app**: eframe with the shared device, the viewer paint callback,
  `ViewState` (orientation, lens, the projection switch), transport and
  scrub, panes in `egui_tiles`, settings, notices, the overlay, the smoke
  driver, kittest on lavapipe.
- **cli**: `probe`, `doctor`, `frame` (the parity reference), `mcp`,
  `call`, the D27 contract (exit 0 to 4, `--json`, SIGINT, `--overwrite`).
- **agent**: A1's tools; `check`'s rules (D46) validate exports.
- **testkit**: Tier 2 gates, benchmark recording, PSNR/SSIM, FNV-64.
- **synth**: synthetic clips with planted answers (the grid clip with its
  magenta marker, the seam clip, sync flash and beep, `jitter-dups.mp4`).


## 3. The project model (`core::project`)

PLAN puts the model and `Edit` in `spherecut-core`. The model uses the
sphere crate's view vocabulary (`Projection`, `Ypr`, quaternions), so
**core gains dependencies on sphere and glam** (D28 is updated: sphere
has never depended on core, so there is no cycle). Nothing in
`core::project` does I/O except `save` and `load`, which go through
`core::fs` (D16).

### 3.1 Shape

The JSON shape is M5's timeline from the start, with M2's limits checked
on apply and on load, so M5 widens the limits instead of changing the
format.

```rust
pub struct Project {
    /// Raised by every applied step, undo and redo included, never
    /// lowered; A2's `if_revision` compares it.
    pub revision: u64,
    /// The next id to hand out. Ids are never reused, so undo never
    /// lowers it.
    next_id: u64,
    pub media: Vec<Media>,          // sorted by id
    pub sequences: Vec<Sequence>,   // M2: exactly one
    pub targets: Vec<Target>,       // look-at targets, project-wide
}

pub struct Media {
    pub id: MediaId,
    /// Relative to the project file's folder when saved (section 5.2),
    /// absolute in memory.
    pub path: PathBuf,
    /// What the file was when it was added, to say a relinked or
    /// changed file isn't the same (section 5.3).
    pub fingerprint: Fingerprint,   // size, duration, first video stream's
                                    // size and rate, FNV-64 of the first MiB
    pub video: MediaVideo,          // size, rate, projection as detected
}

pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub rate: FrameRate,            // the clip's rate by default
    pub output: Output,
    pub tracks: Vec<Track>,         // M2: one video track
    pub markers: Vec<Marker>,       // sorted by time, then id
    pub export: ExportSettings,     // the last export's choices (section 8)
}

pub struct Track { pub id: TrackId, pub kind: TrackKind, pub clips: Vec<Clip> }
                                    // M2: one clip

pub struct Clip {
    pub id: ClipId,
    pub media: MediaId,
    /// The trim: the source's [in, out), in the source's own time.
    pub source: Range,              // Range { start: Time, end: Time }
    /// Where the clip starts in the sequence. M2: always 0.
    pub at: Time,
    pub view: ViewAnim,             // section 6
}

pub struct Marker { pub id: MarkerId, pub at: Time, pub name: String }

pub enum Output {
    /// A virtual camera's picture.
    Flat { aspect: Aspect, height: u32 },      // 16:9, 9:16, 1:1, 4:5;
                                               // 1080, 1440, 2160 tall
                                               // (9:16 and 4:5: that wide)
    /// The sphere, trimmed and turned to a fixed heading and level.
    Equirect { width: u32, orientation: Ypr }, // height = width / 2
}
```

- **Ids** are typed newtypes over `u64` (`ClipId`, `KeyId`, `MarkerId`,
  `TargetId`, `MediaId`, `SequenceId`, `TrackId`), all from the one
  `next_id`, so an id names one thing for the project's life. In JSON
  they are numbers below 2^53, which every JSON reader holds exactly.
- **Times** are `Time` (exact rationals), never floats. A key's time is
  in its clip's source time, so trimming never moves a key off the
  moment it was set on, and M5's ripple edits move clips without
  touching their keys. Markers are in sequence time.
- **M2's limits**, checked by `apply` and by `load` with errors that say
  so: one sequence, one video track, one clip, `at` = 0, flat or
  equirect output. A project from a later Spherecut that exceeds them
  is refused with its schema number (section 5.1), never half-read.
- **Invariants** `Project::validate` checks after every apply and on
  load: ids unique across the project and below `next_id`; every
  reference resolves; `source.start < source.end`, both within the
  media's duration; keys of a track strictly increasing in time; every
  quaternion finite and unit length (normalised on input, refused when
  near zero); FOV within the projection's range (`Projection::max_hfov`
  and `MIN_HFOV_DEG`, which moves from the app to sphere, section 13); names under 200 characters with no control
  characters; at most `MAX_KEYS` (10,000) keys per track and
  `MAX_MARKERS` (1,000) markers per sequence.

### 3.2 Opening media without a project

Opening a file (the GUI's Files pane, `spherecut path/to/clip.mp4`, or
a drop) makes an untitled project: one media, one sequence at the
clip's rate with output flat 16:9 1080p, one track, one clip over the
whole file, the view the viewer has now as a single key on each track.
The title bar says "Untitled" until it is saved. Opening a `.spherecut`
file opens the project.


## 4. Edits, undo and the revision (`core::project::edit`)

### 4.1 The `Edit` enum

One enum, `#[serde(tag = "op", rename_all = "snake_case")]`, versioned
with the project schema. Each variant names ids, never positions.

| Edit | What it does | Refused when |
|---|---|---|
| `add_media {media}` | adds a media entry | id taken |
| `remove_media {id}` | removes it | a clip uses it |
| `add_clip {clip}` | adds a clip to a track | M2: the track has a clip |
| `remove_clip {id}` | removes it with its keys | M2: it is the only clip |
| `set_trim {clip, start?, end?}` | the source in and out | out of the media, or start ≥ end |
| `set_output {sequence, output}` | flat aspect and height, or equirect width and orientation | sizes off the list (section 8.1) |
| `set_rate {sequence, rate}` | the sequence rate | not a rate `FrameRate` takes |
| `set_export {sequence, export}` | the stored export choices | an unknown codec name |
| `add_key {clip, track, key}` | a key on a track (section 6) | a key at that time already |
| `set_key {clip, key, value?, at?, ease?}` | changes a key in place | the new time collides |
| `remove_key {clip, key}` | removes a key | it is the track's last key |
| `set_orient_mode {clip, mode}` | Level or Free for the clip's orientation track (section 6.2) | never |
| `set_view_path {clip, points, replace}` | many keys at once: `{at, orientation, fov}` per point, replacing the keys in the points' span when `replace` | points out of order |
| `add_marker {sequence, marker}` / `set_marker {…}` / `remove_marker {…}` | markers | name rules |
| `add_target {target}` / `set_target {…}` / `remove_target {id}` | look-at targets (section 6.5) | a key still aims at it |
| `rename {id, name}` | a sequence's or marker's name | name rules |

`set_view_path` is A2's "one edit sets a whole view path" (PLAN A2);
M2's GUI uses it for nothing yet, but its tests are in M2 with the rest.
`Edit` is `Clone + Debug + PartialEq + Serialize + Deserialize`. Values
inside edits are the model's own types, so an agent's edit is checked
by the same code as the GUI's.

### 4.2 `Project::apply`

```rust
impl Project {
    /// Apply `edits` in order, all or none. On success the project's
    /// revision is one higher and `Applied` holds the edits as applied
    /// (ids filled in) and their inverse; on error the project is
    /// unchanged and the error names the edit's index and why.
    pub fn apply(&mut self, edits: &[Edit]) -> Result<Applied, EditError>;
}
pub struct Applied { pub edits: Vec<Edit>, pub inverse: Vec<Edit> }
```

- **All or none** by working on a clone and swapping it in when every
  edit and the final `validate` pass. Projects are kilobytes (10,000
  keys are about 1 MB), so the clone costs microseconds to a
  millisecond; a test holds `apply` of one edit on a project with
  10,000 keys under 5 ms on the cloud box.
- **Ids**: an `add_*` edit may leave its id out; `apply` fills it from
  `next_id` and returns the filled edit in `Applied::edits`, which is
  what undo history keeps, so redo re-creates the same ids.
- **Inverse**: each variant's inverse is computed from the state before
  it (a `remove_key`'s inverse is the `add_key` with the key's old id,
  value and ease; a `set_*`'s is the old values). Inverses apply in
  reverse order.
- **No I/O**: `apply` never touches the file system. `add_media` takes a
  `Media` made by `Media::from_probe` (media's probe, called by the GUI
  or CLI before building the edit).

### 4.3 Undo history (`core::project::history`)

```rust
pub struct History { steps: Vec<Step>, cursor: usize, limit: usize }
pub struct Step {
    pub label: String,             // "Trim", "Key view", "Move key"
    pub origin: Origin,            // Pat (the GUI or CLI) or an agent by name
    pub edits: Vec<Edit>,          // as applied
    pub inverse: Vec<Edit>,
    pub before: u64, pub after: u64, // revisions
}
pub enum Origin { User, Agent { name: String } }
```

- `History::record(project, label, origin, edits)` applies and pushes a
  step, dropping any redo steps above the cursor; `undo` and `redo`
  apply a step's inverse or edits as a new revision. The limit is 1,000
  steps; the oldest are dropped.
- **Gestures**: a drag (the viewer, a key in the curve editor, a trim
  handle) applies its edits to a working copy every frame for the
  preview, and records one step from the project as it was when the
  drag started when the button is released (`History::gesture`), so a
  drag is one undo step. A drag cancelled with Escape restores the
  start.
- **Labels and origin** go into the step for A2 and A3; the GUI's Edit
  menu shows "Undo Move key".
- Steps are `Serialize`, so A2 can save history beside the project; M2
  keeps history in memory only and says so in PROGRESS.

### 4.4 Property tests

With proptest strategies for projects (one clip, 0 to 50 keys per track,
markers, targets) and for edit sequences (valid and invalid mixed):

- applying a random sequence of steps, then undoing all of them, gives
  back the original project (equal but for `revision` and `next_id`);
- undo then redo gives the same project as before the undo;
- a rejected edit changes nothing, revision included, and its error
  names its index;
- save then load gives an equal project (section 5);
- every successful apply leaves `validate` passing;
- ids handed out are unique over a run, including across undo and redo.

Run at `PROPTEST_CASES=100000` before the slice merges (the push rule).


## 5. The project file (D47, Pat's choice pending)

### 5.1 Format

PLAN leans to JSON and Pat has been asked (JSON in a `.spherecut` file,
RON, or SQLite; JSON recommended). The plan is written for JSON; if
Pat picks another, only this section and `save`/`load` change.

```json
{
  "format": "spherecut.project",
  "schema": 1,
  "saved_by": "spherecut 0.2.0",
  "project": { "revision": 41, "next_id": 97, "media": [...], "sequences": [...], "targets": [...] }
}
```

- UTF-8, pretty-printed with two-space indents and a fixed key order (the
  struct's), ending in a newline, so a `git diff` of two saves shows the
  change.
- Times as `{"exact": "30030/1001", "seconds": 30.03}` (core's
  `TimeRepr`); `load` reads `exact` and ignores `seconds`, so a hand
  edit of `seconds` alone is refused with a message saying to edit
  `exact`. Quaternions as `[x, y, z, w]`, with a `ypr` field in degrees
  beside them that `load` ignores, for people and agents reading the
  file. Floats are written shortest-exact and read with serde_json's
  `float_roundtrip` feature, so every `f64` loads back bit for bit
  (section 6.6's last test needs it).
- `schema` is checked first: a newer schema is refused ("saved by a newer
  Spherecut, schema 3; this one reads up to 1"), an older one migrated
  step by step (`migrate_1_to_2`, …), each migration with a test from a
  checked-in file.
- Unknown fields are refused (`deny_unknown_fields`), so a typo in a
  hand edit is caught rather than dropped.

### 5.2 Paths

Media paths are saved relative to the project file's folder
(`../Videos/VID_….insv`), made with path components, never by string,
and read back against the folder the project was loaded from. A path
on no common prefix but `/` is still written relative (`../../…`). On
load a missing file is not an error: the project opens with that clip
marked offline, and the Info pane says which file and where it was
looked for (relinking a moved file is M5's media bin).

### 5.3 Fingerprints

A media file that exists but whose fingerprint differs (another size,
duration, stream size or rate, or first-MiB hash) opens with a warning
naming what differs; keys and trims stay, since their times may still
fit. Hashing the first MiB costs a read of 1 MiB per clip on load.

### 5.4 Saving

`save` serialises, writes through `TempFile` beside the target and
renames over it (`persist_replace`, D16), so a crash leaves the old file
or the new one. A save to a path whose folder changed rewrites the media
paths relative to the new folder. The GUI saves on `Ctrl+S` and asks
about unsaved changes on close; autosave is M5's.


## 6. Keyframes and interpolation (`sphere::spline`, `core::project::anim`, D50)

The research note's findings, in short: every reframer checked (Insta360
Studio and app, GoPro's FX Reframe, Reframe360XL, Fusion's PanoMap) keys
yaw, pitch, roll and FOV as separate numbers with per-segment curves;
squad is C¹ only when keys are evenly spaced (with uneven spacing the
speed jumps at a key by up to 2.8× the chord speed); a non-uniform
Catmull–Rom tangent fed into Kim–Kim–Shin's cumulative cubic Hermite
passes through every key and is C¹ at any spacing; easing is a per-key,
per-side scale on the key's velocity; deliberate long-way turns need an
explicit per-segment winding.

One more measurement decides the default. Interpolating along SO(3)'s
geodesics tilts the horizon whenever yaw and pitch change together:
from (yaw 0°, pitch 0°) to (90°, 30°) the slerp midpoint has roll
−6.5°; from (0°, −30°) to (120°, 30°), −17.6°; from (0°, 0°) to (170°,
60°), −31°. A reframed pan with a tilting horizon is the first thing a
viewer notices, which is why the tools above interpolate angles. So:

### 6.1 Tracks and keys

```rust
pub struct ViewAnim {
    pub orientation: Track<Orient>,
    pub fov: Track<f64>,            // hfov in radians (section 7.1)
    pub projection: Track<Projection>,
    /// How orientation keys are joined: Level (default) or Free.
    pub orient_mode: OrientMode,
}
pub struct Track<V> { pub keys: Vec<Key<V>> }   // at least one key
pub struct Key<V> {
    pub id: KeyId,
    pub at: Time,                   // the clip's source time
    pub value: V,
    pub ease: Ease,                 // section 6.3
    /// Extra whole turns of yaw (Level) or about the rotation's axis
    /// (Free) on the segment from this key to the next; 0 is the
    /// shortest way. Orientation keys only.
    pub spin: i8,
}
pub enum Orient {
    Free(Quat),                     // a view orientation, D33
    Aim { target: TargetId, roll: f64 },  // look at a target, section 6.5
}
```

Before the first key a track holds the first key's value, after the last
the last's. Times are converted to f64 seconds from the clip's source
start for the maths only; the model keeps them exact.

### 6.2 Orientation: Level and Free

- **Level** (the default): each orientation key is decomposed into D33's
  yaw, pitch and roll (`Ypr::from_quat`), and the three are interpolated
  as numbers by the scalar spline below, yaw and roll unwrapped so each
  segment takes the shortest way plus its `spin` turns of yaw. A segment
  between two level keys (roll 0) stays level throughout; a pure yaw pan
  is the same path as the geodesic. Pitch is clamped by the spline to
  its keys' range (below), so it stays in [−90°, 90°]. Near the poles
  (|pitch| > 85° at both ends, the tiny planet) yaw and roll lose
  meaning, and D33 folds the heading into yaw there, so a tiny-planet
  key interpolates its heading as yaw.
- **Free**: the research note's spline on SO(3), for moves the angles
  can't do well (a barrel roll, a path through the zenith): keys as unit
  quaternions, segment rotation `δ_i = log(q_{i+1} q_i⁻¹)` on the short
  branch plus `spin`·2π along its axis, non-uniform Catmull–Rom angular
  velocities `ω_i = (Δ_i ρ_{i−1} + Δ_{i−1} ρ_i)/(Δ_{i−1} + Δ_i)` with
  `ρ_i = δ_i/Δ_i`, clamped (`ω_i = 0` when the chords turn back, else
  `|ω_i| ≤ 3·min(|ρ_{i−1}|, |ρ_i|)`), and each segment evaluated in the
  cumulative cubic Hermite form
  `q(u) = q_i·exp(B̃₁(u)A)·exp(B̃₂(u)M)·exp(B̃₃(u)C)` with
  `A = q_i⁻¹(Δ_i/3·ω_i^out)q_i`, `C = q_{i+1}⁻¹(Δ_i/3·ω_{i+1}^in)q_{i+1}`,
  `M = log(exp(−A)·q_i⁻¹q_{i+1}·exp(−C))` on the branch nearest
  `q_i⁻¹δ_iq_i − A − C`, and `B̃₁ = 1−(1−u)³`, `B̃₂ = 3u²−2u³`,
  `B̃₃ = u³`. It passes through every key exactly and is C¹ wherever a
  key's in and out velocities match.

The mode is per track in M2 (`set_orient_mode`, one undo step; the
curve editor and the Inspector show it). Per-segment modes would need
the angular velocity converted between angle rates and SO(3) at the
join, singular at the poles; they wait until someone needs them.

**The scalar spline** (yaw, pitch and roll in Level, FOV in log space,
a target's path, the morph): cubic Hermite per segment with the same
non-uniform Catmull–Rom slopes and the same clamp, which is
Fritsch–Carlson's: monotone data stays monotone, so a pitch between
keys never passes the keys' own range, and keys 0/90/90/0 hold at 90
instead of overshooting to 101.

### 6.3 Easing

Each key has an `Ease { into: Side, out: Side, hold: bool }`, and each
side is `Smooth` (the spline's velocity at that key), `Linear` (the
chord's: the adjacent segment's average rate) or `Stop` (zero). At a
track's first and last key `Smooth` means `Stop`, so a move between two
keys starts and ends at rest. `hold` keeps the key's value until the
next key, then jumps. The presets the GUI offers, named by what the
camera does at the key:

| Preset | into, out | At the key |
|---|---|---|
| Smooth (default) | Smooth, Smooth | passes through without a jolt: C¹ |
| Stop | Stop, Stop | arrives, stops, leaves from rest: C¹ |
| Arrive slowly | Stop, Smooth | velocity jumps from 0: C⁰ |
| Leave slowly | Smooth, Stop | C⁰ |
| Linear | Linear, Linear | constant speed on each side; a two-key Linear segment is exactly slerp (Free) or linear angles (Level): C⁰ |
| Hold | –, – , hold | the value jumps at the next key |

Projection keys default to Stop, so a morph from one projection to the
next follows smoothstep; FOV and orientation keys to Smooth. Per-segment
time warps (the Insta360 app's "Quick In/Out", "Transition Delay") are
not in M2.

### 6.4 Evaluation and the curves

`ViewAnim::at(t) -> (DQuat, Lens)` evaluates the three tracks:
orientation as above; FOV through the scalar spline on `ln(hfov)`, so a
zoom is even in apparent size; projection: between keys of the same
projection that projection, between different ones a `Lens` with `from`,
`to` and `morph` = the segment's 0-to-1 shape from the two keys' sides
(Stop–Stop is smoothstep), `hold` switching at the next key. Lookup is a
binary search on the keys, then one segment's formula.

The curve editor's yaw, pitch and roll (section 10.4) are `Ypr` of the
evaluated orientation sampled per frame plus at every key's exact time,
yaw and roll unwrapped against the previous sample, starting from the
first key with each segment's spin, so a deliberate turn shows as yaw
rising past 180°. Where |pitch| > 85° yaw and roll are drawn broken and
greyed, and not draggable. Dragging a key's yaw rebuilds its quaternion
from (yaw′, pitch, roll) of that key's own decomposition.

**Sticky spin.** A drag that carries a key across 180° from its
neighbour (in the viewer or the curve editor) would make the shortest
way switch sides and the path jump. While a drag runs, each frame's
edit sets the adjacent segments' `spin` so that the segment's rotation
moves continuously from the previous frame's (`anim::follow_spin`), so
the path keeps going the way it went. The drag ends as one step with
those spins in it.

### 6.5 Look-at targets

```rust
pub struct Target {
    pub id: TargetId,
    pub name: String,
    /// The target's direction over the source's time, as (yaw, pitch)
    /// of a view looking at it: one key for a fixed point, more for a
    /// moving one (A2's "follow the rider" sets these).
    pub path: Track<Aim>,           // Aim { yaw: f64, pitch: f64 }
}
```

A key `Orient::Aim { target, roll }` looks at the target, level but for
its `roll`. Between two aim keys on the same target the view is the
target's direction at each time with roll interpolated, so the camera
follows it exactly. A segment between a free key and an aim key is an
ordinary segment whose aim end has the target's direction and the
target's rate at that time (the scalar spline's slope, or for Free the
angular velocity of the aim, by a symmetric difference of 1 ms), so the
join is C¹ and passes through both keys. A target's pitch is clamped
to ±89° for aiming, since a level view of the zenith has no heading.
Targets are project-wide and shown on the sphere overview; a key's
target is chosen from the Inspector, and `Shift`+click on the overview
makes a target at that point.

### 6.6 Property tests

On random tracks (2 to 12 keys, random spacing with ratios up to 20:1,
random values including pairs 179° to 181° apart, random eases and
spins), in both modes:

- every key is hit: the value at a key's time equals the key, within
  1e−9 rad;
- C¹ at every key whose sides match: the one-sided angular velocities
  (finite differences at h = 1e−6 s) differ by at most 1e−5 of the
  segment's chord speed;
- no flips: over 1,000 samples per segment, the angle between successive
  samples is at most 3× the largest chord speed times the step, so no
  sample jumps to the far side;
- spin: a segment with spin n turns |δ| + 2π|n| in total, within 1e−6;
- sticky spin: moving a key by 0.01° across 180° from its neighbour,
  with `follow_spin`, moves every sample by less than 1°;
- Level keeps level: a segment between two roll-0 keys has roll 0 at
  every sample (within 1e−12), and pitch stays within its keys' range;
- Linear Free segments equal slerp, Stop–Stop Free segments equal slerp
  at smoothstep(u), both within 1e−12;
- look-at: between two aim keys the view's forward vector is the
  target's direction within 1e−9; a free–aim join is C¹;
- evaluation is identical after save and load (bit for bit).

Run at `PROPTEST_CASES=100000` before the slice merges.


## 7. Rendering for export (`spherecut-render`, D49)

### 7.1 The output frame and the FOV

A key's FOV is the **output frame's horizontal field of view**, D33's
`hfov` with `p.x = ±1` at the output's left and right edges, so what is
keyed is what is exported whatever the viewer's size. In reframer mode
the viewer draws the output frame as the largest rectangle of the
output's aspect that fits, mapped to `p ∈ [−1, 1] × [−1/a, 1/a]`, and
keeps drawing the same lens past its edges (|p.x| > 1) dimmed to 40 %,
so Pat sees what is just outside the frame. `ViewUniform` gains a
`frame: [f32; 4]` (the rectangle in pixels) and a dim factor; outside
reframer mode the frame is the whole viewer, as in M1, and the existing
uniform-offset test covers the new fields.

Changing the output's aspect (16:9 to 9:16, say) keeps each key's
horizontal FOV, which makes a portrait frame look zoomed in. The
Inspector offers "keep the vertical field of view", which rewrites every
FOV key in one undo step (`hfov' = 2·atan(tan(hfov/2)·a'/a)` for
rectilinear; the projection's own θ(ρ) for the others, solved for the
same vertical edge angle).

### 7.2 Equirect output

`project.wgsl` gains an output kind (the uniform's padding word becomes
`out_kind: u32`): 0 is the lens (M1), 1 is equirect, where pixel
`(x, y)` of a `w × h` target is the direction at longitude
`(x + ½)/w · 2π − π` and latitude `π/2 − (y + ½)/h · π` (D33's equirect
convention, `sphere::equirect::to_dir`), turned by the output's fixed
orientation (`Output::Equirect.orientation`, D33's `world = q · view`).
The LOD comes from the same central differences as the lens. A CPU
reference row and a golden for each of: identity (output equals input
resampled), a 90° yaw (columns shifted by a quarter), and a 30° roll.

### 7.3 The YUV pass

The encoders take Y′CbCr, and the CPU should not convert 4K frames, so
a compute pass (`yuv.wgsl`) turns the linear target into the encoder's
layout in a storage buffer:

- per pixel, the sRGB OETF (the curve the viewer's encode pass applies,
  D31, so an export matches the viewer), then BT.709's matrix to Y′CbCr
  in limited range (16 to 235 and 16 to 240 at 8 bits, ×4 at 10);
- chroma 4:2:0 with left siting (H.273 chroma_loc 0, the H.264/HEVC
  default): horizontal taps ¼, ½, ¼ centred on the even column, the two
  rows averaged;
- rounded to the nearest code, ties to even, as the encode pass rounds;
- written as NV12 (8-bit) or P010 (10-bit, the code in the top 10 bits
  of a little-endian `u16`), each row padded to a multiple of 256 bytes,
  Y plane then the interleaved CbCr plane, so the buffer maps straight
  into an `AVFrame`'s planes with one `memcpy` per row.

Widths must be multiples of 4 and heights even; every preset is
(section 8.1). The stream is tagged BT.709 primaries, transfer and
matrix, limited range, chroma left (D30's tags), the same that decode
assumes for an untagged SDR file, so an exported file decodes back to
its own codes. Sources in BT.2020 or HDR are exported as SDR BT.709 in
M2 (M7 does colour) and the export window says so.

Tests: the pass against a CPU reference on a 64×32 target of every
8-bit code and a 10-bit ramp (exact), both layouts; a synthetic
equirect exported at identity in equirect output comes back within 1
code of its source codes before encoding (the round trip through the
EOTF, f16 and the OETF, D31's tolerance).

### 7.4 Motion blur

Off by default (PLAN). When on, each output frame is the average in
linear light of `n` projections of the same source picture at view
times spread evenly over the shutter (180° by default: half a frame,
centred on the frame's time), so a fast pan smears and a still view is
unchanged. `n = clamp(⌈s / 1.5⌉, 1, 32)`, where `s` is how far, in
output pixels, the frame's centre moves over the shutter, measured from
`ViewAnim::at` at the shutter's two ends. Samples are rendered into the
`Rgba16Float` target one at a time and added by a small compute pass
into an `Rgba32Float` storage texture (no float blending feature
needed), which the YUV pass then reads divided by `n`. A golden on the
grid clip panning at 360°/s at 30 fps, against the CPU reference
averaging the same `n` views, with D32's rules, and a control that a
still view renders the same with blur on and off.

### 7.5 What the viewer evaluates

Each UI frame the viewer asks `ViewAnim::at(source_time)` for the
presented frame's time (section 6.4), unless a look-around or a drag
holds the view (section 10.2). `at` is pure maths on a few keys, well
under a microsecond per call (a bench holds it under 5 µs for a track of
10,000 keys, by binary search), so the UI thread's budget is untouched.


## 8. Export (`spherecut-export`, D52)

A new crate, `export`, depending on core, sphere, media and render (D28
is updated); cli's `render` and the app's export window drive it. It
has `#![forbid(unsafe_code)]`: what needs FFmpeg's C API goes in media's
`ffi`, as today.

### 8.1 Presets

Every size is a multiple of 4 wide and even tall (section 7.3).

| Preset | Size | High / Medium / Small, Mb/s at 30 fps, H.264 |
|---|---|---|
| `flat-16x9-1080`, `-1440`, `-2160` | 1920×1080, 2560×1440, 3840×2160 | 12/8/5, 24/16/10, 45/35/20 |
| `flat-9x16-…` | 1080×1920, 1440×2560, 2160×3840 | as 16:9 of the same pixels |
| `flat-1x1-…` | 1080², 1440², 2160² | ×0.6 of 16:9 |
| `flat-4x5-…` | 1080×1350, 1440×1800, 2160×2700 | ×0.7 of 16:9 |
| `360-3840`, `360-5760`, `360-7680` | 2:1 | 45/35/20, 80/60/40, 160/100/60 |

The bitrates start from YouTube's upload guidance (4K30 SDR 35 to 45
Mb/s, 8K30 80 to 160), are scaled by rate/30 for other rates, by 0.7
for HEVC and 0.6 for AV1, and are checked in the slice against the
parity floor (section 11): a preset whose High fails the floor on the
synthetic clips gets more bits, recorded in D52. They also give A2's
free-space estimate.

### 8.2 The pipeline

Three threads joined by bounded channels, each checking the export's
`StopToken`:

1. **Decode.** media's decoder for the clip's file, the path chosen as
   `frame --decoder auto` and playback choose it (D39, D25: on odin
   Vulkan Video on the shared device; VA-API with the copy path or
   zero copy where doctor says it is faster; software otherwise), an
   exact seek to the trim's start (D29), then frames in order.
2. **Render** (the GPU). For output frame `n`, sequence time `n/rate`
   and source time `t = in + n/rate`; the source frame shown at `t` is
   the last with a presentation time at or before it (frame hold, so
   VFR and a sequence rate unlike the source's both work). Upload it as
   playback does (the copy path or belt, the DMA-BUF import, or path
   B's copier, all render's own APIs), evaluate `ViewAnim::at(t)` (and
   the shutter's samples, section 7.4), project into a target the
   output's size, run the YUV pass, copy into one of 4 readback
   buffers, and map it. In the GUI this uses the app's device and
   submits through the submit gate (D41), as the uploader does, so the
   viewer's presents are not held up; headless it opens its own `Gpu`.
3. **Encode and mux.** Each mapped buffer fills an `AVFrame` (one
   `memcpy` per row); for VA-API it is uploaded into a surface with
   `av_hwframe_transfer_data` (section 8.3); packets go to FFmpeg's MP4
   muxer through `av_interleaved_write_frame`, with the audio (section
   8.5) interleaved.

The number of frames is the count of `n` with `n/rate < out − in`,
computed exactly. A frame the decoder can't give (damage past the
last good frame) fails the export with its time, never a silent hold.

### 8.3 Encoders

Per codec, in order, the first that opens and passes a one-frame probe
(doctor's encode probe, `hwprobe::encode_one`, extended to each codec):

| Codec | Hardware (VA-API) | Software |
|---|---|---|
| H.264 | `h264_vaapi`: 8-bit only, at most 4096×4096 on VCN 4 | `libx264` |
| HEVC | `hevc_vaapi`: Main and Main10, up to 8192×4352, no B-frames on VCN 4 | `libx265` |
| AV1 | `av1_vaapi`: 8- and 10-bit, up to 8192×4352, no B-frames on VCN 4 | `libsvtav1` |

From the research note (Mesa 26.2.5, Linux soc21.c): VA-API encode needs
a frames context (`format=VAAPI`, `sw_format=NV12|P010`); the device
alone is refused. Rate control is CQP, CBR or VBR (QVBR on new
firmware); ICQ is never offered. `compression_level` is Mesa's bit field
(valid, preset speed/balanced/quality/high quality, pre-encode, VBAQ).
av1_vaapi's default QP is on a 0 to 255 scale, so it is always set.
Consequences:

- **360 above 4096 wide in hardware is HEVC or AV1 only.** H.264 at
  5760 or 7680 wide goes to libx264 (level 6.0, which many hardware
  decoders can't play), and the export window says both things before
  it starts; the 360 presets default to HEVC.
- **Rate control**: VA-API in VBR at the preset's bitrate with
  `maxrate` 1.5× and `compression_level` from a table measured on odin
  in slice M2.5 (quality preset, pre-encode and VBAQ for High; balanced
  for Medium and Small); software in CRF (x264 18/21/24 at `medium`,
  x265 20/23/26 at `fast`, SVT-AV1 28/32/36 at preset 8) with VBV
  `maxrate` 1.5× the preset's bitrate. A keyframe every second, closed
  GOPs; B-frames where the encoder has them.
- **10-bit**: a 10-bit source exports 10-bit (P010) in HEVC and AV1,
  8-bit in H.264; the export window can force 8-bit.
- **Fallback**: an encoder that fails to open, or fails within its
  first second of frames, is closed and the export restarts on the next
  in the list, and the result says `fallback: {from, reason}`. A later
  failure fails the export.
- Vulkan Video encode (FFmpeg 7.1+ for H.264 and HEVC, 8.0+ for AV1;
  RADV only with new encoder firmware or `RADV_EXPERIMENTAL`) and
  mapping DMA-BUFs into VA-API surfaces without a copy are not in M2:
  readback is 373 MB/s at 4K30 and 1.3 GB/s at 7680×3840, small beside
  either machine's bandwidth (M8's performance work revisits it).
- API differences across 6.1 to 9.0 that touch this (`AV_PROFILE_*`
  over `FF_PROFILE_*`, `AV_FRAME_FLAG_KEY` over `key_frame`,
  `avcodec_get_supported_config` from 7.1 over `AVCodec.pix_fmts`,
  gone in 9.0) go behind media's version cfgs, and compat builds them
  (the push rule).

### 8.4 The MP4

FFmpeg's mov muxer with `movflags=+faststart` (the moov first, which
YouTube asks for), written to a `TempFile` beside the output. For 360,
after the muxer finishes, `inject` adds `st3d` (mono) and `sv3d`
(equirect, with Spherecut as the metadata source) to the video track,
`SA3D` to an ambiX audio track, **and** the spatial-media V1 `uuid` box
(the XML that Google's injector writes by default), since the research
found no evidence that YouTube reads V2 alone; `spherical.rs` already
parses V1, and the export check reads both back. Video carries no edit
list (its first frame at time 0); audio carries the one its trim needs
(section 8.5). YouTube's guidance says "no edit lists", but FFmpeg's own
AAC encoder writes one for its priming in every file, so this is
untested rather than avoided: Pat's first upload of an export is the
test (section 17).

### 8.5 Audio

The export window's and `--audio`'s choices, for the clip's first audio
stream (an ambiX track if there is one):

- **Copy** (the default when exact): packets copied without decoding,
  re-stamped onto the output's timeline. The start is exact in every
  FFmpeg we build against: the copy starts one packet before the trim's
  start for AAC (80 ms for Opus), those packets get negative times, and
  the muxer writes an `elst` whose `media_time` skips exactly to the
  start. The end is exact in 9.0 (`AV_PKT_DATA_SKIP_SAMPLES` end padding);
  before 9.0 the muxer ends the edit on a packet boundary (up to 21.3 ms
  late at 48 kHz), so after muxing `inject` patches the `elst`
  segment duration in place to the exact end (a same-size field). PCM
  is cut at the exact samples with no edit list. Other codecs are not
  copied: the window greys Copy and explains.
- **AAC**: decoded (media's audio decode), resampled to 48 kHz,
  encoded with FFmpeg's `aac` at 256 kb/s stereo or 384 kb/s for
  4-channel ambiX, which is labelled `4.0` (the encoder refuses an
  ambisonic layout) with `aac_is=0` and `aac_pns=0` so intensity stereo
  and noise substitution don't smear the components; a test plays four
  tones, one per channel, through it and checks they come back in order.
- **None**.

Tests check, with our own decode (which applies an edit's end itself,
since FFmpeg's demuxer keeps the crossing packet whole), that exported
audio starts and ends within one sample of the trim on 6.1, 8.1.3 and
9.0.2 (compat's Tier 1).

### 8.6 Atomic output, cancel and progress

- The file is written as `TempFile` next to the output and becomes the
  output only after the checks (section 9) pass, with `persist_new`, or
  `persist_replace` under `--overwrite` (D16); cancel, an error, a full
  disk or a crash leave no file at the output. A leftover temporary file
  from a killed export is removed by the next export into that folder
  (its name pattern is ours, D16).
- **Free space**: before starting, the export compares the folder's
  free space (`statvfs`) with the preset's `maxrate` × duration × 1.1
  plus the audio, and refuses with both numbers when short (A2 builds on
  this).
- **Cancel** stops all three threads within 2 s (a test), drains nothing
  into the file, and removes the temporary file.
- **Progress** reports frames muxed (only increasing), the total, frames
  per second over the last 2 s, elapsed and estimated time left, the
  encoder and any fallback, at most 4 times a second to the GUI and
  once a second to `--json`.
- **Errors** are one enum (`ExportError`) naming the stage (decode,
  render, encode, mux, metadata, check, file) and the time of the frame
  involved where there is one, with the underlying error's text.


## 9. `spherecut render` (D51)

```
spherecut render PROJECT -o OUT [--preset NAME] [--codec h264|hevc|av1]
    [--encoder auto|vaapi|software] [--quality high|medium|small]
    [--range IN..OUT] [--draft] [--audio copy|aac|none] [--motion-blur]
    [--overwrite] [--json]
```

- Renders the project's one sequence with its stored export settings
  (section 8.1); each flag overrides one of them for this run and is not
  saved. `--preset` names a row of section 8.1's table
  (`flat-16x9-1080`, …, `360-7680`). `--range` takes two `--time`
  values (TimeSpec) in sequence time; `--draft` renders at half the
  height with the fastest settings, for a quick look.
- **D27's contract**, as `frame` has it: exit 0 on success; 1 when the
  export fails or a check does; 2 for usage (an unknown preset, a range
  outside the sequence); 3 when a GPU or an encoder the run needs is
  missing (`no_gpu`, or `--encoder vaapi` with no VA-API encoder for the
  codec, while `auto` falls back and says so); 4 on Ctrl+C
  (`cancelled`). A project that doesn't load is exit 1 with its error's
  code. An existing `OUT` is refused (exit 1, `output_exists`, as
  `frame` does) without `--overwrite`; with it the old file stays until the new one is
  finished and validated, then is replaced atomically. Ctrl+C, a crash
  or a full disk leave no partial file at `OUT` (section 8.6).
- **`--json`** prints one JSON object a line on stdout:
  `{"schema": "spherecut.render/1", "event": "start", "frames": N,
  "encoder": "hevc_vaapi", "size": [w, h], …}`, then at most one
  `{"event": "progress", "frames_done", "frames_total", "fps",
  "elapsed", "eta"}` a second, frames done only ever increasing, and a
  last `{"event": "result", "ok", "output", "frames", "duration",
  "bytes", "encoder", "fallback"?, "checks": [...], "error"?}`. Without
  `--json`, one human line a second on stderr and a summary at the end.
  `docs/cli-json.md` gains the schema, with a test that every event's
  fields are listed (as A1's `docs.rs` does for limits).
- **Checks** after the file is written and before it is moved into
  place: it re-opens through media's probe, has the frame count and
  duration asked for (within one frame), the codec, size and rate asked
  for, and for 360 the st3d and sv3d (and SA3D for ambiX) that
  `spherical::read_tracks` reads back as written. A failed check is a
  failed export (exit 1) and leaves no file.


## 10. The GUI: the reframer

### 10.1 Layout

M1's tiles stay (Viewer, Info, Doctor, Files). M2 adds:

- **A menu bar**: File (Open, Save, Save as, Export…, Close), Edit (Undo
  "label", Redo "label"), View (panes, overlay, settings). Shortcuts:
  `Ctrl+O`, `Ctrl+S`, `Ctrl+Shift+S`, `Ctrl+E`, `Ctrl+Z`, `Ctrl+Shift+Z`
  and `Ctrl+Y`.
- **The timeline strip**, under the viewer in place of M1's scrub bar:
  the whole source with the trim's [in, out) bright and the rest dimmed,
  draggable in and out handles, the playhead, markers (a flag and a
  name; drag to move, double-click to rename), and three key lanes
  (orientation, FOV, projection) with a diamond per key. Clicking a
  diamond selects it and seeks to it; dragging moves it in time (one
  undo step, snapped to frames, `Shift` to snap to markers and the
  playhead); `Delete` removes the selection; right-click gives the ease
  presets. Keys outside the trim are drawn hollow: they shape the curve
  at the trim's edges but aren't in the export. `I`, `O` set in and out
  at the playhead; `M` drops a marker; `K` keys the view; `Alt+[` and
  `Alt+]` jump to the previous and next key.
- **Curves** (a tab beside Info): section 10.4.
- **Sphere** (a tab): section 10.5.
- **Inspector** (a tab): the view at the playhead in numbers (yaw,
  pitch, roll, FOV, projection, morph), editable when on a key; the
  selected key's time and ease; the trim's in, out and duration; the
  sequence's output and rate; the targets list.
- **Export** (a window): section 10.6.

### 10.2 Keyed view and look-around

During playback and scrubbing, the viewer shows the view the keys give
at the presented frame's source time (`ViewAnim::at`), so what plays is
what exports. When Pat moves the view (drag, scroll, a projection
switch):

- with the playhead on a key's time (within half a frame), the drag
  edits that key: one undo step when the drag ends;
- on a track with one key and nothing else (the untitled project's
  start), the drag edits that key, so a static reframe needs no `K`;
- otherwise the view becomes a look-around: the viewer shows it, a badge
  says "Not keyed: K to key", and the next play, seek or step drops it
  and returns to the keyed view. `K` keys it: one `add_key` per track
  (orientation, FOV, projection) at the playhead, one undo step.

So nothing is keyed by accident, and a key is always one press away.
The overlay's view line says which of the three the viewer shows.

### 10.3 Playing the trim

Play stops at out (or loops between in and out with `L` toggled), and
play from beyond out starts at in. Steps and seeks still reach the whole
source, so a trim can be widened. The scrub cache shading (M1) stays.

### 10.4 The curve editor

Hand-drawn with egui's painter (section 13: no plotting crate needed),
x the clip's source time (zoom with `Ctrl` + scroll, pan with a drag of
the background), y per curve with its own unit:

- **Yaw, pitch and roll**, in degrees, sampled from the orientation track
  (section 6.4: yaw unwrapped so it never jumps 360°, pitch in
  [−90, 90], roll unwrapped) at one sample per two points of width,
  with a dot at each key. Dragging a key's dot up or down changes that
  angle of that key only (its quaternion rebuilt from the edited `Ypr`);
  where |pitch| > 85° yaw and roll are drawn broken and greyed and their
  dots are not draggable (section 6.4; edit in the viewer instead).
- **FOV** in degrees, with its keys.
- **Projection**: a band per segment naming the two projections, and
  the morph from 0 to 1 drawn as a curve, its shape the segment's ease.
- Each key's ease is set from its right-click menu (section 6.3) and
  shown by the curve's shape; the selected key's in and out tangents
  are not handles in M2 (presets only, section 6.3).

### 10.5 The sphere overview

An equirect thumbnail of the frame at the playhead, 2:1, drawn by the
viewer's own projector in flat mode (no second decode), with on top:
the camera's footprint (the output frame's edge projected to the sphere,
a closed curve, red), its centre, the path of the centre over the trim
(a polyline at one sample per frame, thinned to 600 points), each key's
centre as a dot, and each look-at target as a cross with its name.
Clicking the thumbnail turns the view to look there, keeping it level
(roll 0): a look-around, or a key edit on a key, as section 10.2 says.

### 10.6 Export

A window with: the preset (section 8.1's rows, grouped Flat / 360),
codec (H.264, HEVC, AV1; the ones this machine can encode first, as
doctor finds them), quality (High, Medium, Small), audio (Copy, AAC,
None, with Copy greyed for a codec it can't trim exactly, section 8.5), motion blur (off by default),
range (the trim, or in to out of the selection), and the output file
(name field and the Files pane's folder, `.mp4` added). Start runs the
export as a background job (section 8.6): a progress bar with frames,
fps and time left, and Cancel. The UI keeps drawing at refresh while it
runs (a kittest test, section 11). When it ends, a notice says where
the file is, the encoder used and any fallback, and the checks; a
failure says what failed and leaves no file. One export runs at a time;
the window can be closed and reopened while it runs.

### 10.7 What the GUI doesn't do in M2

Listed in PROGRESS as unavailable, not stubbed: multiple clips and
tracks (M5), autosave (M5), relinking moved media (M5), stabilisation
and horizon (M4), audio mixing (M6), colour (M7), the render queue
(M8), tangent handles on keys, tracking a subject automatically (M9).


## 11. Tests

Tier 1 (cloud box, lavapipe, software encoders from the system FFmpeg
6.1 and the compat builds, which include libx264 and libx265):

- **core::project**: section 4.4's property tests; `validate`'s rules
  one by one; every `Edit` variant applied, inverted and refused; JSON
  save/load of a checked-in project file per schema, and a newer schema
  refused; relative paths through `..`; a missing and a changed media
  file on load.
- **sphere::spline and ViewAnim**: section 6.6's property tests.
- **render**: the YUV pass against a CPU reference (every code, both
  depths, both layouts); equirect output against the CPU reference;
  the motion blur golden and its control (section 7.4); identity export of a
  synthetic equirect round-trips its codes within 1 code before
  encoding.
- **export**: parity (below) flat and 360 with libx264 and
  libx265 at the high preset; the frame count and timing of a
  29.97 fps and a VFR source; trims that start mid-GOP and mid-audio
  packet; audio copy, AAC and none; ambiX through with SA3D; cancel
  within 2 s leaving no file; a full disk (a small tmpfs, Tier 1 where
  the box allows a mount, else a fake writer that fails) leaving no
  file; an encoder that fails to open falling back to software and
  saying so; every error with its context.
- **Parity tests** (PLAN): decode the export and compare each sampled
  frame (ten per export, and every frame of a 2 s export) against
  `render_still` of the same view at the same time: PSNR ≥ 40 dB
  luma at the high preset for H.264 and HEVC (measured in the slice and
  set just below the worst seen, never above), SSIM ≥ 0.98; negative
  controls that must fail: a view off by 1° of yaw, and frames shifted
  by one.
- **cli**: `spherecut render` through D27's contract: exits 0 to 4,
  `--json` events in order with frames only increasing, `--overwrite`,
  Ctrl+C leaving no file and exit 4, a project that doesn't load (2).
- **app** (kittest on lavapipe): open a clip, `I`/`O`, `K` at three
  times, play and see the view follow the keys (the viewer's uniform
  read back per frame); a key dragged in the strip is one undo step;
  undo/redo through the menu; save and reopen gives the same project;
  the look-around badge and its return on play; the curve editor's yaw
  drag changes only that key's yaw; the overview's click turns the view
  there; export from the window with progress reaching 100 % while UI
  frames stay within the D40 budget (the overlay's UI time, p99 under
  the refresh interval) on lavapipe at a small size.

Tier 2 (hw-test on odin and huginn):

- `export_vaapi_matches_software`: the same project through hevc_vaapi
  and libx265, both against `render_still`, each within its floor.
- `bench_export_4k30_vaapi`: a reframed 4K30 export from the 5.7K
  synthetic clip and from the X5's LRV-stitched sample where present,
  frames per second, with the gate at ≥ 30 fps (PLAN's real-time
  criterion), and the stage times (decode, render, readback, encode).
- `export_360_8k_vaapi`: 7680×3840 HEVC (or AV1 where the encoder takes
  it, section 8.3), its metadata read back, and `check youtube_360`
  passing.
- The GUI smoke run gains an export of 2 s while the UI plays, gated on
  presented ≥ 99 % and UI p99 within refresh, as gui_smoke's gate.
- The toolkit check (section 12).


## 12. The toolkit check (D26)

A prototype, `spherecut-app`'s `timeline_proto` (a feature-gated
binary, `cargo run -p spherecut-app --features proto --bin
timeline-proto`, and a `--smoke` mode as the app has), draws a timeline
of 6 tracks with 500 clips of random lengths (seeded), each a rounded
rect with a name and a thumbnail placeholder, a ruler, a playhead that
moves at 30 fps, the app's overlay with UI time per frame (as PLAN
asks), and:

- drag a clip with snapping to clip edges and the playhead (at most 4
  snap candidates drawn);
- ripple trim of a clip's edge, moving every later clip on its track;
- zoom from the whole timeline to 2 s on screen, and pan.

The smoke mode drives these for 20 s with synthetic input (drags across
the whole width, a zoom sweep) and writes D40's JSON: UI time per frame
(p50, p99, max), presented frames, and frames late. The gate: UI p99
under the refresh interval and presented ≥ 99 % at 1.0 and 1.25 scale
on odin, and at 1.0 and the laptop's scale on huginn, as hw-test's
`gui_proto` test. It runs twice: on odin in slice M2.2b, before the
GUI work, and at the end of M2 in M2.8 as PLAN's criterion (section
15). If it fails, the report names the slow part (layout,
tessellation, painting) from egui's own frame counters, PROGRESS writes
it up, and Pat decides (D26) before anything changes.


## 13. Dependencies

No new third-party crates are expected. What changes inside the
workspace (D22's rows and D28's layout are updated in the slice that
makes each change):

- **core** gains `spherecut-sphere`, `glam` (already in the workspace,
  with its `serde` feature) and `serde_json` (a dev-dependency today) for
  the project file; the workspace's `serde_json` turns on
  `float_roundtrip` (section 5.1). sphere does not depend on core, so there is no cycle.
- **sphere** gains `spline` (section 6) and takes `MIN_HFOV_DEG` from the
  app's `view.rs`, since `validate` needs it.
- **export**, the new crate (section 8), depends on core, sphere, media
  and render, with `#![forbid(unsafe_code)]` like every crate but media
  and render (D28).
- **cli** gains `render` (section 9); **app** gains the reframer GUI
  (section 10) and the `proto` feature with its `timeline-proto` binary
  (section 12), which adds no dependency.
- **egui_plot** (0.38 supports egui 0.37) is not used: the curve
  editor's needs (three angle curves with unwrapping, broken segments
  near the poles, keys as draggable dots, a band per projection segment)
  are a few hundred lines on egui's painter, and the timeline strip is
  drawn the same way.
- The software encoders come from the FFmpeg we link: libx264, libx265
  and libsvtav1 in the cloud box's 6.1 and in RPM Fusion's 8.1.3 on odin,
  libx264 and libx265 in the compat builds. Fedora's own `ffmpeg-free`
  has none of x264 and x265, and doctor says so (section 15, M2.5).
  `cargo deny` sees no change: they are FFmpeg's, not ours.


## 14. Risks, most serious first

1. **Real-time export on huginn.** PLAN's hardware criterion is a
   reframed 4K30 export at or above real time on both machines. huginn
   decodes H.264 and HEVC with VA-API only (its RADV has no Vulkan Video
   for them), with the copy path unless zero copy proves faster there
   (D25). Its M0 bench decoded 5760×2880 that way at 121 fps, but it
   hasn't run H1's engine benches, and on odin the same copy path took 31
   ms (p50) per 5.7K 8-bit frame in the engine and 110 ms at 10-bit,
   against a 33 ms frame at 30 fps. Export adds render, readback and
   encode on top. Mitigation: the pipeline overlaps the three stages (section 8.2),
   the bench records each stage's time so the slow one is named, and
   zero copy or path B is used where doctor says it is faster. If huginn
   still falls short, PROGRESS says by how much and which stage, and M8
   takes VA-API surface mapping (no download) as its first item.
2. **VCN encode throughput is unmeasured.** No run has encoded more than
   one frame on either machine, and 7680×3840 at 30 fps is 3.6× the
   pixel rate of 3840×2160 at 30. Mitigation: the M2.5 bench measures it on odin first; 8K is not part
   of PLAN's real-time criterion (only 4K30 is), so a slower 8K export
   is reported, not a failure.
3. **egui with a real timeline.** D26's question. The timeline strip of
   M2 is light (one clip), but M5's is not, and if egui fails, every GUI
   line written in M2 is at risk. Mitigation: the prototype is built and
   run on odin in the second slice, before any GUI work (section 15),
   and run again at the end of M2 as PLAN asks.
4. **Exact audio trims across FFmpeg versions.** Copying audio with an
   exact start relies on the muxer's edit list from negative timestamps,
   and an exact end before 9.0 on our own `elst` patch. Players differ
   in how they read edit lists. Mitigation: our own decode checks both
   ends on 6.1, 8.1.3 and 9.0.2 (section 8.5), the AAC re-encode is
   always available, and a player that ignores the edit list plays at
   most one packet (21.3 ms) of extra audio.
5. **Spline and look-at complexity.** Two orientation modes, eases, spin
   and aim keys multiply the cases. Mitigation: one function per mode
   with the property tests of section 6.6 at 100,000 cases, a CPU-only
   module with no GUI, and the research note's numeric scripts as the
   reference the tests were derived from.
6. **YouTube's reading of the metadata and edit lists.** The research
   found no first-hand evidence that YouTube reads the V2 boxes alone,
   and its guidance says no edit lists while FFmpeg's AAC writes one in
   every file. Mitigation: V1 and V2 metadata both (section 8.4), video
   with no edit list, A1's `check youtube_360` on every 360 export's
   test, and Pat's first upload as the real test (section 17).
7. **Memory at 8K.** A 7680×3840 `Rgba16Float` target is 236 MB and the
   motion blur accumulator 472 MB, beside the decoder's surfaces and the
   frame cache. Mitigation: the export allocates them once per export
   and frees them after; with the GUI open, the frame cache's budget
   (D34) is lowered by the export's own use while it runs, and an export
   that cannot allocate says how much it needed.


## 15. Slices

Each slice is a branch from main (or a stack on `m2/start`), ends with
the push rule (the check log, compat, touched proptests at 100,000) and
merges with `--no-ff`. A fresh review runs at the end of M2 (M2.9),
and after any slice whose diff is large enough to need one.

| Slice | What | Done when |
|---|---|---|
| **M2.1** | `core::project`: the model (section 3), `Edit` and `apply` (4.1, 4.2), `History` (4.3), JSON save and load (5), `validate`; sphere's `MIN_HFOV_DEG`; core's new dependencies | section 4.4's property tests and section 11's core tests pass; a checked-in schema 1 file loads |
| **M2.2** | `sphere::spline` (Level, Free, the scalar spline, eases, spin, `follow_spin`) and `core::project::anim` (`ViewAnim::at`, targets, aim keys) | section 6.6's property tests at 100,000; the `at` bench under 5 µs |
| **M2.2b** | The toolkit prototype (section 12) and its Tier 2 test, then an odin run by Remote Control at 1.0 and 1.25 | the report is in PROGRESS; a failure goes to Pat before M2.6 starts |
| **M2.3** | render: the output frame and dim (7.1), equirect output (7.2), the YUV pass (7.3), motion blur (7.4) | their goldens and CPU-reference tests on lavapipe |
| **M2.4** | `spherecut-export` with the software encoders: the pipeline (8.2), presets (8.1), audio (8.5), the MP4 and metadata V1 and V2 (8.4), atomic output, free space, cancel, progress (8.6) | the parity tests, flat and 360, with libx264 and libx265; audio trims exact on the three FFmpeg versions; cancel and full-disk tests |
| **M2.5** | `spherecut render` (section 9) with `cli-json.md`; the VA-API encoders, the encoder choice and fallback (8.3); doctor lists the encoders it can open; hw-test's Tier 2 export tests and bench, then an odin run | D27's contract tests; odin's `bench_export_4k30_vaapi` at ≥ 30 fps or the slow stage named |
| **M2.6** | GUI part 1: project open, save and close, the menu bar, undo and redo, the timeline strip (trim handles, markers, key lanes), keyed view and look-around, trim playback, the output frame in the viewer (10.1 to 10.3) | section 11's app tests for these |
| **M2.7** | GUI part 2: the curve editor (10.4), the sphere overview (10.5), the Inspector, targets, the export window (10.6), gui_smoke's export | the remaining app tests, including the UI budget during an export |
| **M2.8** | Hardware: the full hw-test on odin, the toolkit check again, and huginn's prompt for Pat (huginn's runs wait for Pat, as M1's did) | odin's results in PROGRESS and perf.md |
| **M2.9** | Docs (D47 to D53, ARCHITECTURE, PROGRESS, README, `cli-json.md`, PLAN ticks), a fresh review of M2 against PLAN, its fixes, the note to Pat | the review's findings fixed or listed as known |

Then A2 (PLAN), whose plan starts from section 4's `Edit` and section 9's
`--json`.

**Why the prototype moves up.** PLAN and D26 put the toolkit check at
the end of M2. It is built and run early as well because a failure
there would put M2.6 and M2.7 at risk, and the prototype needs nothing
from M2.3 to M2.5. The check that counts for PLAN's criterion is still
the run at the end, in M2.8, on the same binary.


## 16. Decisions to record

| D | Title | Slice |
|---|---|---|
| D47 | The project file: JSON (or Pat's choice), the schema number and migrations, relative paths, fingerprints, offline media | M2.1 |
| D48 | The project model and `Edit` in core: ids, revision, all-or-none `apply`, history steps with label and origin, M2's limits; core depends on sphere and glam (updates D28) | M2.1 |
| D49 | Rendering for export: the keyed FOV is the output frame's, equirect output, the YUV pass and its tags, motion blur (updates D31) | M2.3 |
| D50 | Keyframes: Level by default and Free per track, the scalar and SO(3) splines, eases, spin, look-at; why not squad and why Level (the horizon measurements of section 6) | M2.2 |
| D51 | `spherecut render`: flags, the `spherecut.render/1` events, the checks before the output appears (extends D27) | M2.5 |
| D52 | The export pipeline: the crate, threads, presets and bitrates, encoder order and fallback, rate control, the MP4 and its metadata, audio copy and its edit list | M2.4, M2.5 |
| D53 | The toolkit check's prototype and its result (updates D26) | M2.2b, M2.8 |


## 17. Questions for Pat

1. **The project file format** (asked, JSON recommended). The plan is
   written for JSON; another choice changes only section 5 and `save`
   and `load`.
2. **Level by default** (for information, not a question). PLAN says
   orientation is interpolated on SO(3). The horizon tilt between keys
   that change yaw and pitch together (up to 31° in section 6's
   measurements) makes M2 interpolate yaw, pitch and roll by default, as
   every reframer checked does, with SO(3) as the per-track Free mode.
   Keys are still stored as quaternions.
3. **A YouTube upload of a 360 export** when Pat has time: the only way
   to know that YouTube reads the metadata and accepts the audio's edit
   list. A private upload is enough.
4. The toolkit check's result, if it fails (D26 says Pat decides).


## Appendix A. Not in M2

Each is listed in PROGRESS as unavailable, not stubbed:

- more than one clip, track or sequence (M5's timeline, which this
  format already describes);
- autosave, relinking moved media and a media bin (M5);
- stabilisation and horizon levelling from the gyro (M4);
- audio mixing, gain and ambisonic rotation (M6); M2's audio is copy or
  re-encode of one stream;
- colour grading, HDR and BT.2020 output (M7); M2 exports SDR BT.709;
- a render queue, presets beyond section 8.1, and Vulkan Video encode
  (M8);
- tracking a subject (M9; A2's `set_view_path` stands in);
- tangent handles on keys and per-segment orientation modes;
- saved undo history (A2);
- export of a stereo (top-bottom) 360 file: M2 exports mono only, and a
  stereo source exports its left eye with a notice.
