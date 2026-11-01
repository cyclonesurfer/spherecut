# Spherecut sample request

These clips let me verify each camera's raw format against real files instead of
guesses. Record them **in-camera** and copy them off the card untouched: no
trimming, no re-muxing, no phone app transfer. Trimming or app transfer can drop
the metadata trailer, gyro data, and calibration that Spherecut needs.

Keep each clip short (10–20 s) unless noted. Copy the **whole set of files** each
recording produces, including `.lrv` / `.LRV` low-res companions and, for
Insta360 modes that write one file per lens, both `.insv` files.

Put them in `samples/<camera>/` in the repo checkout (it is git-ignored), for
example `samples/insta360-x4/`, `samples/gopro-max2/`, `samples/dji-osmo360/`.
Add a short `samples/<camera>/NOTES.txt` with the camera model, firmware
version, and one line per clip saying which mode it was.

The mode names below are my best understanding of current firmware. If a mode
doesn't exist on your camera, skip it and note what the nearest one is called.

## For your main camera (full set)

| # | Mode | Why |
|---|------|-----|
| 1 | Highest 360 resolution, 30 fps (or 29.97), standard/flat color, default audio | The baseline raw format |
| 2 | Same resolution at 24 or 25 fps if offered | Frame-rate and timebase handling |
| 3 | The main 60 fps 360 mode (e.g. 5.7K60 / 5.6K60 / 6K60) | Common high-frame-rate path, often a different file layout |
| 4 | The highest-frame-rate 360 mode (e.g. 4K100/120) | Different resolution, sometimes different lens-file split |
| 5 | Mode 1 again with the log profile (I-Log, GP-Log, D-Log M) and 10-bit if it's a separate option | 10-bit decode, log metadata, LUT work later |
| 6 | Mode 1 with 360/ambisonic audio **on**, 30 s, talk while walking a full circle around the camera | Ambisonic channel layout and orientation |
| 7 | Mode 1 with 360 audio **off** (stereo/default) | Audio variant |
| 8 | Tripod, static, 30 s: camera about 1.5 m from a wall or fence with straight lines and text crossing the side seams, plus something at 3–5 m | Stitching and seam calibration |
| 9 | Handheld or selfie stick, 60 s: walk, turn quickly, tilt the camera past vertical, and do two full flips (roll it over end to end) | Gyro sync, stabilization, horizon through flips |
| 10 | On the tripod, slowly rotate the camera by hand: a full turn in yaw, then pitch it forward ~90° and back, then roll ~90° and back | Gyro-to-video time sync and axis conventions |
| 11 | Any single mode with in-camera stabilization/FlowState/HyperSmooth **off**, and the same with it **on** if that changes what is recorded | Whether the raw file is already stabilized |
| 12 | One clip from 1 exported through the vendor's own app/desktop software as stitched equirect (any resolution) | Reference for comparing my stitch |
| 13 | One recording longer than the camera's file-split point (usually ~4 GB, often 8–10 min at max resolution) | Chaptered/split recordings. Optional, it's large |

## For stitching the X5 (M3, added 2026-11-01)

Your seven X5 takes settle the file layout. These four settle the lens
geometry and the seams, which clip 8 can't do alone: its wall is 1.5 m away,
so parallax hides small calibration errors. Same rules as above: in-camera,
copied untouched, with the `.lrv`.

| # | Take | Why |
|---|------|-----|
| 14 | 8K30 on the tripod outdoors in an open space, the camera level, everything in view at least 10 m away. Hold 15 s, then turn the camera about 90° on the tripod, level it again and hold 15 s more. Long horizontal lines (the horizon, roof edges, a distant fence) **and** vertical ones (trees, poles, building corners) at both of the camera's sides, where the seams run. Please note in `NOTES.txt` the height from the ground to the lenses, and that nothing stood nearer than 10 m | Fits the lens scale and angles without parallax, and tells which of the camera's two calibrations to trust. The horizontal lines measure the seam's roll, the vertical ones the scale; the second heading and the height let the fit separate the lenses' angles from the ground's nearness |
| 15 | 8K30, 10 s: cover the lens on the screen side with your hand for the first 2 s, then take it away | Which video track is which lens |
| 16 | 5.7K at 29.97 fps (or the nearest 5.7K mode), 20 s, anything | The 5.7K30 real-time target |
| 17 | 8K30 handheld, 30 s: walk from deep shade into direct sun and back, with one lens facing the sun | Matching exposure and white balance between the lenses |

If you agree to it (question 2 of `docs/design/m3-plan.md`), clip 12 for
take 14: a stitched equirect export of it from Insta360 Studio. Spherecut
only compares its own stitch against it when the fit leaves a question open;
it never fits to it, and nothing from Studio goes into the code. Still
missing from the first set, if they're easy: clips 5 (I-Log or 10-bit), 6
(360 audio on) and 11 (FlowState off and on).

## For your other cameras (short set)

Clips 1, 3, 6, 8, 9 and 12 from the table above. If one camera only gets a
stitched-export workflow (e.g. DJI, if raw turns out to be undocumented),
clip 12 plus one raw file of clip 1 is still enough for me to look at the raw
format.

## Not needed

Photos, timelapse/hyperlapse, bullet time, single-lens (flat) modes, and
in-camera reframed exports. If you shoot one of those regularly and want it
supported, tell me and I'll add it.
