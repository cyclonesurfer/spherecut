# M2 keyframes research: reframer tools, SO(3) splines, easing, curves, look-at

*2026-10-31. For PLAN.md "M2 Clip reframer" and docs/design/m2-plan.md.
Conventions are D33 / `sphere::orient`: q = R_y(−yaw)·R_x(pitch)·R_z(−roll),
world = q·view, −Z forward, +Y up; `sphere::slerp` already takes the short
arc. How the facts below were gathered: the sandbox's egress proxy refused
curl to every web host tried (insta360.com, wikipedia, blender, adobe,
arxiv: 403 connect_rejected), so pages were read with the WebFetch tool,
whose User-Agent I cannot set. No personal data went into any request. Each
claim says what it rests on: **[src N]** is a page I opened (list at the
end), **[num]** is a numeric check (the scripts in `scripts/` beside this
note: numpy, run with `python3 -I scripts/<name>.py`), and **(unverified)**
or **(inferred)** says so.*

*Revised 2026-10-31 after the M2 plan's review (m2-plan amendment 2,
findings 2, 12, 14, 16 and nit 44), with every script rerun. What changed:
the squad and De Casteljau figures are the scripts' own, with the measure
stated (the first version's 2.8× did not reproduce); the "clamped 2.0" of
`c1check2.py` is explained as an artefact of its measure; the clamp's
reversal rule was a step in the keys and is replaced by a continuous
limiter; the C¹ test's tolerance and the spin formula are corrected;
D33's pole fold is at exactly ±90°; the horizon measurements that decide
Level as the default are added (section 2b). Section 5 says how look-at
was built.*

## Short answers

1. **Tools.** Every tool I could check keys the view as Euler-style angles,
   not quaternions. Insta360 Studio's keys are pan, tilt, roll, FOV and
   distortion, with 9 lens presets; the segment between two keys gets a
   transition setting; Deep Track follows a picked subject [src 1–3]. The
   Insta360 phone app has 7 per-segment curves: Linear, Ease In/Out, Ease
   In, Ease Out, Quick In/Out, Transition Delay, Hard Cut [src 4]. GoPro
   Quik "smooths out the transitions keyframe-to-keyframe" and has AI
   subject tracking [src 6]. GoPro FX Reframe (Premiere, AE, HitFilm)
   keys Pan, Tilt, Rotate, Lens and Zoom as separate host parameters
   [src 7, 8]. Reframe360XL for Resolve lerps pitch, yaw, roll and FOV
   separately, with Power, Sine, Expo and Circular blend curves, and mixes
   tiny-planet and rectilinear rays by amount [src 11]. Fusion's PanoMap
   has X/Y/Z dials with a selectable rotation order [src 10]. I found no
   Adobe-made effect called "VR Reframe". The "VR Reframe" I found is
   GoPro's plugin [src 13]. Adobe ships VR Rotate Sphere and VR Sphere to
   Plane. I found no documented ±180° flip complaint for any 360 tool
   (unverified either way). The general Euler failure is documented,
   though: UE Sequencer flips past ±180° (closed "By Design") [src 18],
   and naive Euler lerp goes the long way [src 19].
2. **Spline.** Use a **non-uniform Catmull–Rom angular velocity at each
   key, evaluated per segment in Kim–Kim–Shin's cumulative cubic
   Bézier/Hermite form, fed rotation vectors directly.** It passes
   exactly through every key and is C¹ at any key spacing [num]. Squad
   with Shoemake's inner points is C¹ only with equal spacing. With
   uneven spacing (0.2 to 3 s) the angular velocity jumps at a key by up
   to 2.03× the larger chord speed either side (33× the smaller), and the
   speed changes by up to 14× across the key [num] [src 14].
   The De Casteljau/slerp form of the same spline is C¹ while every
   control rotation stays under 180°; with keys up to 120° apart at
   0.2 to 3 s spacing, 24 of 300 tracks have one past 180° and jump by
   up to 1.8× the larger chord speed [num] [src 14]. KKS cumulative B-splines are C²
   but do not pass through their control points [src 17], so they suit
   smoothing tracker output, not user keys. For signs: take the short
   way by default. Store any deliberate long way or extra turns as an
   explicit per-segment winding, as AE's Rotation and Blender's Euler
   modes allow and their quaternion/Orientation modes do not [src 20, 21].
3. **Easing.** Ease is a per-key, per-side scale on the key's angular
   velocity (0 = ease to a stop), plus an optional per-segment time warp.
   C¹ survives at a key exactly when both sides end with the same
   velocity. "Smooth" on both sides does that, and so does "ease" on
   both sides (zero velocity). Linear or mixed sides give C⁰. Hold is a
   jump. That matches AE: Easy Ease is speed 0 with 33.33% influence,
   and in and out velocities are separate unless locked "Continuous"
   [src 22]. It also matches Blender: Bézier handles default to Auto
   Clamped, which avoids overshoot, and easing modes are separate
   interpolation types [src 23].
4. **Curves.** Sample q(t) per frame (plus the exact key times). Turn
   each sample into ypr with `Ypr::from_quat`, then unwrap yaw and roll
   against the previous sample. Grey out yaw and roll where |pitch| >
   ~85°, since they are ill-conditioned there. Editing a yaw *key*
   rebuilds that key's q from (yaw′, pitch, roll) and adjusts the
   neighbouring windings so the curve stays where it was dragged. Ypr
   tangent handles need the Euler-rate Jacobian, which is singular at
   the poles, so leave them out of v1. Blender shows quaternions as raw
   W/X/Y/Z F-curves ("not intuitive"), and its Euler curves need the
   Discontinuity (Euler) Filter [src 20, 24]. AE keeps Orientation as one
   shortest-path property, separate from per-axis Rotation [src 21].
5. **Look-at.** Make a target a direction on the sphere, keyed and
   interpolated on S² (or fed by a tracker), with roll and FOV on their
   own tracks. aim(d, roll) = R_y(−yaw_d)·R_x(pitch_d)·R_z(−roll). To
   blend smoothly, bake the target into the boundary key: q_i =
   aim(d(t_i), roll_i), and ω_i is the look-at motion's angular velocity
   at t_i. The free↔look-at join is then C¹ through the same Hermite
   machinery. A slerp cross-fade with a smoothstep weight also works, but
   it does not pass through keys. Clamp or special-case targets near the
   zenith or nadir.

## 1. Existing tools (details)

| Tool | Keyed properties | Interpolation / ease | Tracking / look-at | FOV / tiny planet |
|---|---|---|---|---|
| Insta360 Studio (desktop) | "5 sliders (pan, tilt, roll, fov, distortion) and 9 presets" [1] | Transition set by clicking "the line between two keyframes" [2]; "automatically creates a smooth transitions" [1]; type names not documented (unverified) | Deep Track: "locks onto moving subjects like persons, vehicles, or animals", keeps them centred [3]; how it writes keys: undocumented | Presets Default, Crystal Ball, Tiny Planet, Natural, Ultra Wide, Wide, Linear, Linear+, Narrow [1]; keys apply only to flat exports [5] |
| Insta360 app | a key "records all your adjustments … (such as FOV, zoom level, etc.)" [4] | 7 per-segment curves (above); a key inserted mid-segment copies the curve; a key appended at the end gets Linear; deleting a key gives the next segment the earlier curve [4] | ViewFinder records phone motion and swipes [2b]; Deep Track, Snap Wizard, Auto Frame [2c] | tiny planet by zooming out "until it becomes a spherical shape" [2b] |
| GoPro Quik (2025) / GoPro Player | pan and/or zoom keys; Digital Lens profile [6] | "smooth out the transitions keyframe-to-keyframe" [6]; Player's options undocumented (its pages refused the fetch) | "Select a subject and Quik uses … AI to automatically keep that object in view" [6]; MotionFrame records the gyro and touch [6] | horizon levelling in Player [6b] |
| GoPro FX Reframe (Pr/AE/HitFilm) | Pan, Tilt, Rotate, Lens, Zoom, plus source Yaw/Pitch/Roll, X/Y offset, shutter angle, "Sync Keyframes" [7] | the host's per-parameter keyframes; Insta360's tutorial says to pick "Continuous Bezier" for smooth moves [8] | none | Lens slider |
| Insta360 Reframe (V2) plugin | the supported route for .insv in Premiere now; an old project broke when "Insta360 Reframe" was renamed "Insta 360 Reframe V2" [9] | parameter list unverified (Insta360's page refused the fetch) | (unverified) | (unverified) |
| FCP 360 reframing (Keyframe Toolbox) | "Pan, Tilt, Field of View and Roll" (+ Distortion, Opacity, Blur) [12] | not stated | – | – |
| Resolve: Fusion PanoMap | X/Y/Z rotation dials, any of 6 rotation orders [10] | Fusion splines per channel | – | – |
| Resolve: Reframe360XL (OFX, third party) | main Pitch −90..90, Yaw −180..180, Roll, FOV; 20 "aux cameras", each with Pitch/Yaw/Roll/FOV/Rectilinear/Tiny Planet; a keyed "Camera Sequence" [11] | per-parameter `p1 + (p2 − p1)·blend`, with Power/Sine/Expo/Circular blend curves [11] | – | `dir = mix(fisheye, tinyplanet, amt)`, then `mix(dir, rect, amt)` in the shader [11] |
| Adobe immersive effects | VR Rotate Sphere (Tilt, Pan, Roll), VR Plane to Sphere, VR Sphere to Plane (AE) [13b, 13c] | host keyframes; one thread reports VR Rotate Sphere playback ignoring its middle keys [13d] | – | – |

Complaints found: an Insta360 Studio view kept rotating despite a
front-view key, fixed by clearing the cache [src 25]. Premiere's
relative pan/tilt offsets start the next clip "from the wrong starting
point" [src 13e]. The ±180° flip and spin complaints I found are in
general-purpose tools: UE Sequencer [src 18], Blender [src 26], and
Euler lerp in general [src 19]. That the 360 tools spin the long way
from yaw 170° to −170° is **inferred** from their per-angle lerp and
±180° ranges [src 11]. I found no report of it for them. I found no
sources for wobble, overshoot or roll drift complaints in these tools
(unverified).

## 2. Interpolation on SO(3)

**Squad** [src 14]: Squad(p,a,b,q;u) = slerp(slerp(p,q,u), slerp(a,b,u),
2u(1−u)), with Shoemake's inner points a_i = b_i = q_i·exp(−(ln(q_i⁻¹q_{i+1})
+ ln(q_i⁻¹q_{i−1}))/4). Shoemake assumed uniform intervals. The splines
docs call his tangent-continuity proof "allegedly … flawed", with new
proofs in Kim et al. '96 and Dam et al. '98. The docs suggest a
Δ-weighted variant but make no continuity claim for it, and they note
that squad's control points sit far from the curve and "wrap around" the
hypersphere earlier than Catmull–Rom's. **The checks** [num]: random
6-key tracks (300 or 200 of them), with one-sided finite differences at
h = 1e−6 s, whose noise floor is about 1e−5 of the speed. Two measures
appear below: the jump relative to the larger one-sided speed at the key
(`c1check.py`, `c1check2.py`), and relative to the larger chord speed
either side (`squad_jump.py`, `decasteljau_wrap.py`, `clamped_c1.py`). The
first reads about 2 wherever both speeds are near 0, which is noise, not
a jump.

| method | spacing | worst jump | script |
|---|---|---|---|
| squad, Shoemake points | equal, 1 s | 6.3e−5 of the speed (C¹) | `c1check.py` |
| squad, Shoemake points | 0.2 to 3 s | 0.93 of the speed; 2.03× the larger chord speed, 33× the smaller; the speed changes 14× across the key, direction kept | `c1check.py`, `squad_jump.py` |
| CR controls, De Casteljau-slerp | equal | 4.4e−5 (C¹) | `c1check.py` |
| CR controls, De Casteljau-slerp | 0.2 to 3 s, keys ≤ 120° apart | 1.64 of the speed; 1.8× the larger chord speed, only on the 24 of 300 tracks with a control rotation past 180° (3.3e−5 on the others); keys ≤ 30° apart: 1.2e−5 | `c1check.py`, `decasteljau_wrap.py` |
| CR controls, KKS cumulative Bézier | 0.2 to 3 s | 1.85 of the speed (the same controls, so the same wrap) | `c1check.py` |
| CR velocities, cumulative Hermite fed rotation vectors | ratio up to 1, 3, 15 | 5.6e−5, 3.3e−5, 7.9e−6 (C¹) | `c1check2.py` |
| the same, clamped | ratio up to 1, 3, 15 | "2.0" of the speed at keys the clamp stops; 5.8e−6, 4.4e−6, 3.0e−6 of the larger chord speed (C¹) | `c1check2.py`, `clamped_c1.py` |

So squad is G¹ but not C¹ under uneven spacing: the speed jumps at
keys. Blender's Cessen also called squad not useful [src 27].

**Kim–Kim–Shin cumulative form** (SIGGRAPH '95, pp. 369–376 [src 15]):
q(t) = q₀·∏ exp(ω_i·B̃_i(t)), with ω_i = log(q_{i−1}⁻¹q_i) and B̃_i the
cumulative basis (the sum of B_j for j ≥ i) [src 16]. With B-spline bases
this is C² and *approximating*; Sommer et al. say it does not pass
through its control points in general above order 2 and treat only
uniform knots [src 17]. With the cubic Bernstein basis (B̃₁ = 1−(1−u)³,
B̃₂ = 3u²−2u³, B̃₃ = u³) you get a cumulative Bézier. Kim et al. note it
"has a different shape from the Bézier quaternion curve of Shoemake"
[src 16]. On the same controls the two differ by up to 2.7° [num], but
they share the same end values and end derivatives (3× the first/last
control rotation per unit u) [num]. Squad-style tangents, KKS's, and the
splines docs' non-uniform Catmull–Rom [src 14b] can all feed it.

**Recommendation: non-uniform CR tangents + cumulative cubic Hermite**
(exp/log below are full-angle rotation-vector maps: glam's
`DQuat::from_scaled_axis` and `to_scaled_axis`; check which branch
`to_scaled_axis` returns):

```
keys (t_i, q_i), Δ_i = t_{i+1} − t_i
δ_i = shortest log(q_{i+1} q_i⁻¹)                       // world frame, |δ_i| ≤ π
      + winding_i · 2π · δ̂_i                            // per-segment turns, default 0
      (or δ_i − 2π·δ̂_i for "the other way")
ρ_i = δ_i / Δ_i                                          // chord angular velocity
ω_i = (Δ_i ρ_{i−1} + Δ_{i−1} ρ_i) / (Δ_{i−1} + Δ_i)      // CR at interior keys [14b]
limit (like Blender's Auto Clamped; the revised, continuous form):
      ω_i ← ω_i · max(0, cos θ_i)       // θ_i the angle between ρ_{i−1} and ρ_i
      then |ω_i| ≤ 3·min(|ρ_{i−1}|, |ρ_i|)
ends: ω_0 = 0 (ease) or ρ_0; same for ω_{n−1}
per-key ease: ω_i^in = s_in·ω_i, ω_i^out = s_out·ω_i      (s = 1 smooth, 0 ease)
segment i, u = (t − t_i)/Δ_i:
  A = q_i⁻¹ (Δ_i/3 · ω_i^out) q_i                         // body frame at q_i
  C = q_{i+1}⁻¹ (Δ_i/3 · ω_{i+1}^in) q_{i+1}              // body frame at q_{i+1}
  M = (q_i⁻¹ δ_i q_i) − A − C                               // the middle turn, windings kept
  R = log(exp(−M) · exp(−A) · q_i⁻¹ q_{i+1} · exp(−C))     // its small correction
  q(u) = q_i · exp(B̃₁(u)A) · exp(B̃₂(u)M) · exp(B̃₂(u)R) · exp(B̃₃(u)C)
```

(Corrected in M2.2. This note first had one middle rotation,
M = log(exp(−A) · q_i⁻¹ q_{i+1} · exp(−C)) on the branch nearest
(q_i⁻¹ δ_i q_i) − A − C. Where that rotation is near 2πk the branch's
axis is ill-conditioned: at 100,000 property cases a 0.001° move of a
key moved the view by 0.05°. With the turn on the chord's axis and a
small correction R, the same case moves it by about the move.)

q(0) = q_i and q(1) = q_{i+1} exactly (checked to 1e-9 rad). The world
angular velocity is ω_i^out at u = 0 and ω_{i+1}^in at u = 1, so C¹ holds
wherever in and out match [num]. Overshoot: yaw keys 0/90/90/0 reach
101.25° between the 90s unclamped and exactly 90° clamped. Yaw 0@0,
10@0.1, 100@3 stays monotone either way (unclamped it peaks at 100.03°,
clamped at 100.00°) [num]. The clamp is my heuristic, an SO(3) analogue
of Fritsch–Carlson: no source, and it needs its own proptests.
Derivatives are closed-form in KKS form (q′ = q·Σ B̃_i′·…), which helps
the motion-blur sub-frame sampling.

**The first version's reversal rule was a step** (review finding 2). It
set ω_i = 0 when ρ_{i−1}·ρ_i ≤ 0, so where the chords meet at a right
angle (a pan, then a tilt) the sign of a dot product near 0 decides
between full speed and a stop. `clamp_discontinuity.py`: a 60° pan in
2 s, then a 30° tilt in 1 s, with the tilt's yaw nudged by ±0.01°, gives
|ω| = 22.36°/s on one side of the nudge and 0 on the other, and the two
paths differ by up to 6.87°. Scaling by max(0, cos θ) first makes the
velocity continuous in the keys: the same nudge moves the path by at
most 0.020°, and at the right angle itself the camera stops (|ω| = 0;
0.007°/s at +0.01°). A sharp corner in Free mode therefore slows the
camera in proportion to how sharp it is, down to a stop at 90° or more.
The scalar spline (Level's angles, FOV, morph) needs no such factor: its
3·min cap already drives the slope to 0 as either chord does.
`c1check2.py` and `clamped_c1.py` still use the first version's rule.
Their C¹ results hold for the continuous one too, since C¹ needs only
one velocity per key, and their overshoot cases have parallel or zero
chords, where the two rules agree.

**Signs and spins.** Normalising keys is fine. Never canonicalise w ≥ 0
inside evaluation, and take the short arc per segment (`sphere::slerp`
already does). Editors: AE's Orientation "takes a shortcut", can't keyframe
multiple revolutions [src 21]. Blender's quaternion mode "cannot interpolate
between two orientations that are at a distance greater than 180°", so a
spin needs keys "180° from each other at most"; its axis-angle and Euler
modes allow revolutions [src 20]. Suggested for Spherecut: a per-segment
`spin` (shortest | other way | ±N turns) stored in the project and set by an
`Edit`. Keep it **sticky**: when a drag carries a key across the 180° antipode,
the editor flips the segment's spin so the path keeps going the way it went,
rather than snapping to the new shortest way. Exactly 180° apart is the
unstable case behind "flips while editing". A 270° yaw in one segment through
M's long branch [num]: forward yaw 0, 12, 42, 85, 135, 185, 228, 258, 270°.

**Property tests** (proptest, as `sphere` uses): (a) q(t_i) = q_i within
1e-9 rad. (b) At keys with matched sides, the one-sided angular
velocities agree within 1e−5 of the largest of the two one-sided speeds
and the chord speeds either side (plus 1e−8 rad/s), by **second-order** one-sided differences
((4·r(h) − r(2h))/2h at h = 1 µs). First-order differences at h = 1e−6
cannot meet 1e−5 at short spacing: on a correct scalar spline the worst
is 8.9e−5 of the larger chord speed at 1/30 s spacing, and against the
smaller chord speed it fails at every spacing (`c1_tolerance_max.py`,
`c1_tolerance.py`). (c) No flips: angle(q(t), q(t+h)) ≤ K·max_i|ρ_i|·h
over dense samples, with K = 3 when clamped. (c′) Lipschitz in the keys:
moving any key by 1e−3° moves no sample by more than 50× that, on
generators that include pans and tilts meeting at exactly a right angle
(the step rule above fails this; the continuous limiter passes).
(d) Keys 179.99°–180° apart: moving one key by 0.01° moves the path by
less than ~1°, because spin is sticky. (e) A segment with spin n turns
|δ_i + 2πn·δ̂_i| in total, that is | |δ_i| + 2πn |, signed along δ_i's
axis (n = −1 is the long way: +30° with n = −1 turns 330°); where
|δ_i| < 1e−12 the axis is Y, the yaw axis. (f) The same project
evaluates bit-identically after save → load; quaternions are not
renormalised on load, since 38.3% of normalised quaternions change bits
when normalised again (`normalize_idempotent.py`).

## 2b. The horizon: why Level is the default

Interpolating along SO(3) tilts the horizon whenever yaw and pitch change
together. Between two level keys (roll 0), the slerp path's roll
(`horizon.py`, 1,001 samples a path, D33's convention):

| from (yaw, pitch) | to (yaw, pitch) | roll at the midpoint | largest \|roll\| on the path |
|---|---|---|---|
| (0°, 0°) | (90°, 30°) | −6.46° | 6.47° |
| (0°, −30°) | (120°, 30°) | −17.59° | 17.59° |
| (0°, 0°) | (170°, 60°) | −31.11° | 32.48° |

Every reframer in section 1 keys yaw, pitch and roll as numbers, so none
of them tilts a pan like this. Spherecut's Level mode does the same (yaw
the short way plus whole turns, roll the short way, pitch as is, all
through the scalar spline); Free mode keeps the SO(3) spline for moves
the angles can't do (a barrel roll, a path through the zenith). Which is
the default is Pat's call (D50); Level is recommended.

## 3. Easing with a C¹ spline

Two layers. **Per-key velocity scale** (above) decides the continuity:

| key in / out | velocity at key | continuity |
|---|---|---|
| smooth / smooth | ω_i (CR, clamped) | C¹ |
| ease / ease | 0 | C¹ (camera stops) |
| mixed (smooth/ease, linear/smooth …) | differ | C⁰: velocity jumps |
| hold | constant until next key | jump at the next key |

"Linear" at both ends of a segment means the chord velocity ρ_i at both
ends. Because B̃₁+B̃₂+B̃₃ = 3u, that reduces exactly to slerp: constant
angular speed, with a velocity jump at its keys by design. Ease at both
ends (A = C = 0) reduces exactly to slerp at smoothstep(u). Both checked
to 1e-15 rad [num]. **Per-segment time warp** u′ = e(u), for
presets like Insta360's Quick In/Out and Transition Delay [src 4]: the
key velocity becomes ω·e′(0) or ω·e′(1), so C¹ survives where
e′(1) of the left segment equals e′(0) of the right one. Warps with
e′ = 0 at both ends (smoothstep) make every key a stop, which is what a
per-segment-only tool gives. AE: easing is "speed/rate of change …
based, not value-based" [src 21b]; Easy Ease is "a speed of 0 with an
influence of 33.33% on either side"; in and out velocities are separate
unless Continuous is set [src 22]. AE's Orientation can't be split into
dimensions; "the position property is the only property that allows
separate dimensions" [src 21b]. That Orientation keeps its own temporal
speed graph is unverified. Blender has Constant, Linear and Bézier
("smooth in both values and speed"), easing types (Ease In "moving
slowly at the beginning of the curve segment"), and handle types; it
recommends Auto Clamped "to prevent overshoots … (S-shapes)" [src 23].
Blender interpolates quaternion channels per component and then
normalises: "not using the shortest path on a sphere", no constant speed
[src 27]. FOV and morph are scalars: use the same per-key ease on a
monotone cubic (Blender-style Auto Clamped) in log(FOV), so zoom feels
even (suggestion).

## 4. Yaw/pitch/roll curves from the quaternion track

- **Sampling.** Evaluate q at every frame in view, plus the exact key
  times, so keys sit on the curve. Convert with `Ypr::from_quat`. Unwrap
  yaw and roll: y_k += 2π·round((y_{k−1} − y_k)/2π). Start the unwrap at
  the first key, offset by that segment's spin, so the displayed yaw
  rises past 180° during a deliberate spin. Pitch never wraps (±90°).
- **Near the poles.** Yaw and roll lose meaning as |pitch| → 90°.
  `from_quat` folds all heading into yaw only at the pole itself (D33;
  `sphere::orient` tests the level component against 1e−9, so in effect
  at exactly ±90°), and close to it they swing fast in opposite
  directions. Blender warns about "the
  middle axis approach[ing] … 90°" [src 20]. Draw yaw and roll dashed or
  greyed where cos(pitch) < ~0.09 (|pitch| > 85°), and break the line
  instead of drawing the swing. Tiny planet (pitch −90°) is a normal pose
  here, so its key should show roll 0 with the heading in yaw, as D33
  says.
- **Editing.** Only key points are draggable. Dragging key i's yaw sets
  q_i = Ypr(yaw′, pitch_i, roll_i).to_quat(), with pitch and roll from
  that key's own decomposition, and updates the adjacent spins so the
  dragged unwrapped value is honoured. Pitch and roll work the same way.
  At |pitch| > 85°, refuse roll edits or edit yaw ± roll together.
  Tangent handles in ypr would need ω = J(ypr)·(ẏ, ṗ, ṙ). The yaw rate is
  about world −Y, the pitch rate about the yawed right axis, and the roll
  rate about the view axis. J is singular at the poles, so v1 should
  offer only the per-key ease presets.
- **Others.** Blender's quaternion F-curves are four independent W/X/Y/Z
  channels, "not intuitive … difficult to understand and edit" [src 20,
  27]. Its Euler curves are easy to read but flip near 90°, which the
  "Discontinuity (Euler) Filter" cleans up [src 24]. AE: "Orientation
  takes a shortcut", "it's better to use Orientation to 'pose' a layer,
  and … Rotation to animate it", and "if Y Rotation goes beyond +/-90
  degrees, the X and Z values flip by 180" [src 21].

## 5. Look-at targets

- **Representation.** Store a target as a world direction d (a point on
  the sphere, which the overview and equirect UI can show). It is static,
  keyed (slerp on S², or the same CR/Hermite machinery with zero roll),
  or from a tracker. The camera aims at it:
  yaw_d = atan2(d.x, −d.z), pitch_d = asin(d.y), and
  q = R_y(−yaw_d)·R_x(pitch_d)·R_z(−roll)·O.
  O is an optional fixed view-space offset that puts the target off
  centre (rule of thirds). Roll and FOV stay on their own tracks.
  m2-plan's targets are keys of (yaw, pitch) in a media's time, one
  target per media (review finding 11; `remove_target` refuses while a
  key aims at it).
- **Blending free ↔ look-at.** Recommended: an aim key is an ordinary
  orientation key whose q_i and ω_i are computed from the target:
  q_i = aim(d(t_i), roll(t_i)), with ω_i from d′(t_i) and roll′(t_i)
  analytically, or by a symmetric difference. Between two aim keys on
  the same target, use aim(d(t), roll(t)) directly, so the camera follows
  exactly. A free → aim segment is a normal Hermite segment, so the join
  is C¹, passes through the keys, and uses the same easing rules. An
  alternative is q = slerp(q_free(t), q_aim(t), w(t)), with w a
  smoothstep over a window. It is C¹ if both inputs are, but it is
  ambiguous when they are near 180° apart (keep the sign continuous
  frame to frame), and it doesn't hit keys.
- **Poles and wobble.** A level-horizon aim is singular at d = ±Y: the
  camera spins about its view axis. Clamp |pitch_d| ≤ 89°, or switch to
  minimal-rotation (parallel-transport) aiming near the pole, which
  drifts the roll. Tracker output is noisy. Fit a cumulative cubic
  B-spline (C², approximating [src 17]) or low-pass d(t) before aiming,
  or the frame wobbles. What Deep Track does internally is undocumented
  (unverified).
- **As built in M2.2** (`core::project::anim::eval`). A target's path is
  the scalar spline over its keys' yaw (the short way between keys) and
  pitch; its pitch is clamped to ±89° (`MAX_AIM_PITCH_DEG`), and its rate
  is 0 where clamped. An aim key's rate on a Smooth side is analytic, not
  a symmetric difference (which would average the two sides at a target
  key): in Level, the target's spline slope and the roll's; in Free, the
  same rates turned into an angular velocity by `Ypr::omega`. Before the
  first key and after the last, and while it holds, an aim key follows
  its target; two aim keys on the same target follow it with roll from
  the scalar spline.

## Sources (opened)

1. Mantis Sub academy, "How to quickly reframe a 360 video with Insta360 Studio": https://lute-algae-dlda.squarespace.com/academy/how-to-quickly-reframe-360-video-with-studio
2. Insta360 ONE R manual, Studio editing: https://onlinemanual.insta360.com/oner/en-us/studio/editing · 2b. Insta360 blog, How to edit and reframe 360 videos: https://blog.insta360.com/how-to-edit-and-reframe-360-videos/ · 2c. Insta360 forum, 4 ways to reframe: https://forums.insta360.com/section/16/post/60173
3. Insta360 Studio manual, Deep Track: https://onlinemanual.insta360.com/studio/en-us/operation-guide/edit-function/deep-tracking-function
4. Insta360 app manual, Keyframes and camera movements: https://onlinemanual.insta360.com/app/en-us/operation-tutorial/edit-function/keyframes-and-camera-movements
5. Insta360 Studio manual, Keyframe: https://onlinemanual.insta360.com/studio/en-us/operation-guide/edit-function/keyframe
6. GoPro press release (Quik 360 tools, 2025): https://www.placera.se/pressmeddelanden/gopro-gopro-introduces-new-easy-and-powerful-360-editing-tools-to-its-quik-mobile-app-20250624 · 6b. GoPro Player, Mac App Store: https://apps.apple.com/ph/app/gopro-player/id1460836908?mt=12
7. HitFilm manual, GoPro FX Reframe: https://www.manula.com/manuals/fxhome/hitfilm/15/en/topic/gopro-fx-reframe
8. Insta360 support, ReFrame plugin in Premiere: https://www.insta360.com/support/supportcourse?post_id=17187
9. Adobe community, Insta360 Reframe plugin backward compatibility: https://community.adobe.com/questions-729/lack-of-backward-compatibility-in-the-insta360-reframe-plugin-1417619
10. Fusion 18.6 manual, PanoMap: https://www.steakunderwater.com/VFXPedia/__man/Fusion18-6/Fusion18_Manual_files/part1181.htm
11. Reframe360XL README and source: https://github.com/LRP-sgravel/reframe360XL , https://raw.githubusercontent.com/LRP-sgravel/reframe360XL/master/Reframe360.cpp
12. Keyframe Toolbox (FCP), 360 Reframe: https://keyframetoolbox.fcp.cafe/how-to-use/360-reframe/
13. PremiumBeat, GoPro VR effects in Premiere: https://www.premiumbeat.com/blog/gopro-vr-effects-premiere-pro/ · 13b. Adobe, 360 video effects / VR Plane to Sphere: https://www.adobe.com/learn/premiere-pro/web/360-video-effects-vr-plane-sphere · 13c. Adobe community, VR Sphere to Plane: https://community.adobe.com/t5/after-effects-discussions/sphere-video-to-video-proportions-i-see-in-another-app/m-p/11010542 · 13d. Adobe community, keyframes vs VR Rotate Sphere: https://community.adobe.com/t5/premiere-pro/keyframes-not-working-with-vr-rotate-sphere/td-p/11906633 · 13e. Adobe community, GoPro 360 editing help: https://community.adobe.com/t5/premiere-pro-discussions/gopro-360-video-editing-help/td-p/13179985
14. splines docs (M. Geier), Squad: https://splines.readthedocs.io/en/latest/rotation/squad.html · 14b. Non-uniform Catmull–Rom rotation splines: https://splines.readthedocs.io/rotation/catmull-rom-non-uniform.html · 14c. Uniform CR: https://splines.readthedocs.io/en/latest/rotation/catmull-rom-uniform.html · 14d. De Casteljau with slerp: https://splines.readthedocs.io/en/latest/rotation/de-casteljau.html
15. KAIST record, Kim–Kim–Shin, SIGGRAPH '95 pp. 369–376: https://dspace.kaist.ac.kr/handle/10203/116416 (paper PDF not opened; formulas via 16)
16. splines docs, Cumulative form: https://splines.readthedocs.io/en/latest/rotation/cumulative-form.html
17. Sommer et al., Efficient derivative computation for cumulative B-splines on Lie groups (CVPR 2020): https://ar5iv.arxiv.org/html/1911.08860
18. Unreal issue UE-67605: https://issues.unrealengine.com/issue/UE-67605
19. splines docs, Naive Euler interpolation: https://splines.readthedocs.io/en/latest/rotation/naive-euler-angles-interpolation.html
20. Blender manual source, Rotation modes: https://projects.blender.org/blender/blender-manual/raw/branch/main/manual/advanced/appendices/rotations.rst
21. ProVideo Coalition, CMG Hidden Gems ch. 13 (3D space): https://www.provideocoalition.com/cmg_hidden_gems_chapter_13_-_3d_space/ · 21b. Adobe community, separate dimensions for Orientation: https://community.adobe.com/t5/after-effects-discussions/separate-dimensions-for-orientation-property-not-available/m-p/14429931
22. Adobe help, Speed between keyframes: https://helpx.adobe.com/ca/after-effects/desktop/animate-in-after-effects/speed-between-keyframes/speed.html
23. Blender manual source, F-curve properties: https://projects.blender.org/blender/blender-manual/raw/branch/main/manual/editors/graph_editor/fcurves/properties.rst
24. Blender manual PR, Discontinuity Filter text: https://projects.blender.org/blender/blender-manual/pulls/104676/files
25. Insta360 forum, problem with keyframe in Studio: https://forums.insta360.com/section/17/post/32643
26. Blender Artists, animation spinning the wrong way: https://blenderartists.org/t/animation-spinning-the-wrong-way/1290155
27. Blender devtalk, Quaternion interpolation: https://devtalk.blender.org/t/quaternion-interpolation/15883

**Not verified / not opened:** Shoemake 1985/1987 and Dam–Koch–Lillholm
1998 themselves (cited via 14). KKS's paper text (via 16). Insta360
Studio's transition type names and the Reframe V2 parameters (pages
refused the fetch). GoPro Player's ease options (robots-blocked). Whether
any Adobe effect is literally named "VR Reframe". AE Orientation's graph
editor display. Deep Track and Quik tracking internals. **[num]** checks:
the scripts in `scripts/`, numpy with finite differences at h = 1e-6,
which puts the noise floor around 1e-5. Run on 2026-10-31 with numpy
2.5.3; each prints the figures quoted here (the random seeds are fixed).
`c1check2.py`, `squad_jump.py`, `decasteljau_wrap.py` and `clamped_c1.py`
read `c1check.py` or `c1check2.py` from their own folder.
