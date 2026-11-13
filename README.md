# Spherecut

A native Linux editor for 360° video, written in Rust, in early
development. It is being built to open raw files from Insta360, GoPro and
DJI 360 cameras as well as stitched equirectangular video, to reframe them
into flat video or edit them as 360, and to export with the metadata
YouTube and headsets need. It renders with wgpu on Vulkan and decodes
with FFmpeg: in software, or on the GPU through Vulkan Video or VA-API.

It runs offline: no accounts, no telemetry, no network access.

**Status: early.** M3, stitching a camera's raw files, is under way:
M3.0 to M3.4 and M3.8 are in, and stitching itself comes with M3.5 and
later. M0 (foundations) is done. M1 (the viewer) is done, its last
hardware rows waiting on huginn's run. M2 (reframing and export) has
all its code in (M2.1 to M2.10); the project format (Pat's call), and
huginn's 4K30 export row and toolkit check, are open. Track A, the tools
for AI agents: A1 and A2 are in, waiting on runs with a real agent.
What works today:

- `spherecut [file]` opens the viewer: a stitched equirect played on the
  GPU with its audio, looked around by dragging (the point you grab stays
  under the pointer), zoomed about the pointer, shown rectilinear,
  fisheye or as a tiny planet, stepped frame by frame and scrubbed. It
  checks your codecs and GPU at startup and shows the fixes it finds.
  F3 shows the performance overlay, File ▸ Settings keeps the cache,
  decoder, filter, slots and A/V offset, and a camera's raw file with a
  stream a lens shows its lenses flat, side by side, both always at the
  same moment of the clip (M3.8, DECISIONS D67).
- Reframing and export (M2): opening a video makes a project of it. The
  viewer shows the output's frame, and moving the view on a key edits
  it; Alt+K keys the view, I and O set the trim, and the strip under
  the picture moves keys, markers and the trim (Escape lets a drag go).
  The Curves tab draws and drags yaw, pitch, roll, FOV and the
  projection over time, Sphere shows where the camera points, and the
  Inspector edits the numbers and the look-at targets. File ▸ Export…
  (Ctrl+E) exports flat or 360 in the background while you work. File
  saves and reopens `.spherecut` projects, and Edit undoes and redoes
  every change.

- `spherecut probe <file>...` shows what an MP4/MOV or Matroska/WebM file
  is: container, streams, codecs, colour, exact frame rates and
  timebases, spherical video and ambisonic metadata, and the camera
  vendor and layout it most likely came from. Of an Insta360 X5's file
  it also reads the trailer: the camera, which streams hold the lenses,
  their calibration and the clip's companion file (M3.2). It checks
  MP4/MOV files for the kinds of damage listed under Build and run, and
  prints control characters in what it shows of a file, its warnings,
  damage and FFmpeg's lines included, as `\xNN`. `--json` gives the
  same as one JSON document, for scripts and AI agents
  ([docs/cli-json.md](docs/cli-json.md)).
- `spherecut doctor` checks what your FFmpeg, drivers and GPU can actually
  do. It decodes one small frame each of H.264 High, HEVC Main, HEVC Main 10
  and AV1 in software, on VA-API and on FFmpeg's Vulkan device, encodes one
  frame with each VA-API encoder, and lists the Vulkan devices. For each
  problem it prints a fix: the exact `dnf` or `pacman` command where a
  package fixes it. `--file` also checks a camera's file: whether it can
  be stitched and whether both its lenses decode.
- `spherecut frame <file> -o out.png` renders one frame of a stitched
  equirect through a virtual camera (yaw, pitch, roll, field of view,
  rectilinear, fisheye or stereographic, and a morph between two), or
  the whole sphere as an equirect, into a PNG, headless, on the GPU's
  Vulkan driver or Mesa's lavapipe. With `--reference` it stitches an
  Insta360 camera's two lenses on the CPU (the GPU's stitch comes in
  M3.7). The frame is exactly the one asked for, by time or by number.
- `spherecut render <project> -o out.mp4` exports a project's sequence
  as an MP4, encoded on the GPU through VA-API or in software. Every
  export, the window's too, is read back before it is put in place, by
  Spherecut and by FFmpeg's own demuxer.

- `spherecut mcp` lets an AI agent on this machine (Claude Code, Codex,
  Gemini CLI, Copilot CLI, Cursor, Zed, goose, opencode) go through your
  footage: probe a folder, look at any moment from any direction as
  labelled images, measure timing, motion and audio over a clip, check a
  file's 360 metadata against YouTube's or another target, and ask what
  this machine decodes. It reads only under the folders you give it and
  changes no file unless you allow writing in one folder. Then an agent
  can also make a project of a clip, key the view to follow something,
  trim it, undo its own steps, render it in the background and compare
  the file with the project, while the project isn't open in the
  window; the window's undo goes back through the agent's steps too.
  `spherecut call` runs the same tools from a shell, and `spherecut
  agents install-skill` installs the recipes agents follow.
  [docs/agents.md](docs/agents.md) has each client's setup. Spherecut
  itself makes no network requests, but what it returns to an agent,
  frames of your footage included, goes to that agent's model provider.

See `PLAN.md` for the milestones and `PROGRESS.md` for where things stand.

## Requirements

- Linux with an AMD or Intel GPU (Mesa RADV or ANV). NVIDIA isn't supported.
- FFmpeg 6.1 or newer, with its development files, H.264/HEVC decoders,
  an AV1 software decoder (libdav1d), and libx264/libx265, built with
  Vulkan support (the Tier 1 doctor and hardware-refusal tests need
  FFmpeg's Vulkan device).
- Rust 1.95 or newer, with rustfmt and clippy for development.
- clang, for generating the FFmpeg bindings at build time.
- The ALSA library's development files (libasound) and pkg-config, which
  the audio output links. Sound plays through PulseAudio's protocol,
  which PipeWire serves on Fedora and Arch (`pipewire-pulse`), or else
  ALSA; with no sound device, playback is silent.
- For the viewer, a Wayland or X11 desktop session. It loads
  libwayland-client and libxkbcommon (Wayland), or libX11, libXcursor,
  libXrandr, libXi and libxkbcommon-x11 (X11), when its window opens, as
  it loads the Vulkan loader; a desktop has them. With no display,
  `spherecut` exits 3 and says so, and the other commands work as
  before.
- For the Tier 1 tests: Mesa's lavapipe Vulkan driver and strace (see
  Development).

### Fedora (43 or newer)

Fedora's own FFmpeg (`ffmpeg-free`) and its Mesa VA-API and Vulkan
drivers leave out H.264 and HEVC, which nearly every 360 camera records.
Use RPM Fusion's builds
(enable the RPM Fusion free repository first; see rpmfusion.org):

```sh
sudo dnf swap ffmpeg-free ffmpeg --allowerasing  # or, without ffmpeg-free: sudo dnf install ffmpeg --allowerasing
sudo dnf install ffmpeg-devel clang-devel pkgconf-pkg-config alsa-lib-devel libva-utils vulkan-tools mesa-vulkan-drivers
sudo dnf install strace                          # for the Tier 1 tests
# AMD GPUs:
sudo dnf install mesa-va-drivers-freeworld       # or: dnf swap mesa-va-drivers mesa-va-drivers-freeworld
sudo dnf swap mesa-vulkan-drivers mesa-vulkan-drivers-freeworld   # Vulkan Video H.264 and HEVC (RPM Fusion free)
# Intel GPUs (RPM Fusion nonfree):
sudo dnf install intel-media-driver
# Rust with rustfmt and clippy:
sudo dnf install rustup && rustup-init -y
```

### Arch

```sh
sudo pacman -S --needed rust ffmpeg clang pkgconf alsa-lib libva-utils vulkan-tools
sudo pacman -S --needed vulkan-radeon                       # AMD
sudo pacman -S --needed vulkan-intel intel-media-driver     # Intel
sudo pacman -S --needed vulkan-swrast strace                # lavapipe and strace, for the Tier 1 tests
```

### Ubuntu 24.04 (development only)

```sh
sudo apt install pkg-config clang ffmpeg libavcodec-dev libavformat-dev \
  libavutil-dev libswscale-dev libswresample-dev libasound2-dev mesa-vulkan-drivers \
  vulkan-tools vainfo strace
```

Your user needs to be in the `render` group for GPU video
(`sudo usermod -aG render "$USER"`, then log in again). `spherecut doctor`
checks the FFmpeg codecs, the GPU drivers and access to the render node.

## Build and run

```sh
cargo build --release
./target/release/spherecut clip.mp4
./target/release/spherecut doctor
./target/release/spherecut probe VID_20261022_165451_00_004.insv
./target/release/spherecut probe --json clip.mp4
./target/release/spherecut frame clip.mp4 -o out.png --time 12.5 --yaw 30 --fov 90
```

`spherecut probe` exits 0 when every file is healthy, 1 when any file
can't be opened, isn't MP4/MOV or Matroska/WebM, is truncated or corrupt,
or has no video or audio stream, and 2 when the command line doesn't
parse, such as `spherecut probe` with no file.
A damaged file that FFmpeg can open still gets its report, with a
`DAMAGE` line per problem, and stderr gets one line per bad file, such as
`error: clip.mp4 is damaged: <first reason> (and 2 more problems)`. In
MP4/MOV files, probe counts these as damage:

- a top-level box cut off by the end of the file, which the `mp4` line
  names: ``truncated `moov` (4847 of 4947 bytes)``;
- anything the box reader warns about, such as a malformed box, a box
  below the top level whose size is 0 (only the last top-level box may
  have one; anything after it in its parent is read as part of it), and a
  missing `moov` box. The reader looks inside `moov` and small top-level
  containers such as `udta`, not inside fragments (`moof`);
- sample tables that disagree with each other, or chunks outside the
  `mdat` data;
- a box type that isn't printable text inside `moov`, `trak`, `mdia`,
  `minf`, `stbl`, `stsd`, `edts`, `dinf`, `mvex`, `tref`, the sample
  entries probe knows, `sv3d` or `proj`, including four zero bytes
  anywhere but in QuickTime's
  terminator, the last box of a sample entry, which holds only zero bytes;
- a box in `moov` that starts with a track header (`tkhd`) but whose type
  is another printable one, such as `Trak` or `free`: FFmpeg skips it, and
  its stream with it;
- an `sv3d` without its `svhd`, its `proj` or the `prhd` in `proj`, which
  makes FFmpeg ignore the projection;
- a box in a sample entry that starts with a spherical video header
  (`svhd`) but whose type is another printable one, such as `rv3d`: FFmpeg
  skips it, and the projection with it;
- a `mvhd` or `mdhd` timescale of 0, or one FFmpeg reads as negative;
- a video sample entry under an audio handler, or the other way round;
- a video or audio track that FFmpeg reads as another kind of stream;
- a video or audio stream FFmpeg could not set up although its track has
  samples and its sample entry is a codec FFmpeg reads, which usually
  means a damaged `avcC` or `hvcC`;
- a video or audio stream whose decoder refuses to open with its codec
  setup, such as an `esds` that gives a reserved sample rate, although
  FFmpeg filled in the stream's fields from the sample entry.

Damage that keeps the structure consistent, such as changed sample sizes
or times, and damage inside `mdat` are not found without decoding. A
stream whose codec FFmpeg doesn't know, such as a `zzzz` sample entry, is
a `warning` line and doesn't fail probe: it may be a real codec that this
FFmpeg lacks. So is a stream this FFmpeg has no decoder for, a stream
FFmpeg could not set up from an empty track, a stream whose decoder
says it lacks a feature the stream uses (`Function not implemented`, as
for AAC object type 0), and a stream of pictures larger than the 2^27
pixels (16384x8192) Spherecut's decoders make (DECISIONS D24): the
warning gives its size. The `ffmpeg` lines hold the warnings and errors
FFmpeg logged while reading the file, one whole line each, starting with
the context that logged it, such as `[aac]`. A repeated line is listed
once with a count, and at most 32 are listed. They often say what FFmpeg
could not read, but they don't fail probe either, because healthy files
can have some: cover art gives a `timescale not set` line. Only MP4/MOV
structure is checked in full; Matroska files are checked against their
Segment length only, which a `structure` line says.

Probe opens nothing but MP4/MOV and Matroska/WebM files, and FFmpeg reads
only the file it was given. A file FFmpeg would read as anything else,
such as an HLS playlist (`.m3u8`), an ffconcat script or a WAV file, is
refused, with the container's name, before FFmpeg's demuxer for it
runs: ``error: FFmpeg reads list.m3u8 as Apple HTTP Live Streaming
(`hls`), and Spherecut opens only MP4/MOV and Matroska/WebM files``. A
file name is always a file name, even one like `concat:a.mp4` or
`http://host/x.mp4`, and FFmpeg gets no network protocols (DECISIONS
D24).

When a file can't be opened, its `ffmpeg` lines follow the error on
stderr (with `--json`, they are in the file's `error.ffmpeg_log`), since
FFmpeg's log often says why it could not open the file (`moov atom not
found`). File names don't have to be UTF-8; in the text
report and in the JSON, bytes that aren't UTF-8 show as U+FFFD. In the
text report, a control character in anything taken from the file, its
`warning`, `DAMAGE` and `ffmpeg` lines among them, prints as `\xNN`
(ESC as `\x1b`), so that a file can't move the cursor or recolour the
terminal; the JSON has the text as it is. When the
reader of probe's output goes away, as in `spherecut probe *.mp4 |
head`, the output stops quietly, and probe still checks the remaining
files, so the exit code and the errors on stderr cover every file.

`spherecut doctor [--render-node <PATH>] [--file <FILE>] [--json]`
checks the GPU behind the render node hardware decoding uses, which is
`/dev/dri/renderD128` until the GUI knows the GPU it renders on (D30), or the one
`--render-node` names. (`spherecut frame` decodes on the node of the GPU
it renders on, D39.) It exits 1 only when a problem is an error: FFmpeg has no H.264, HEVC, ProRes or AAC
decoder, or nothing decodes AV1. Every decoder and encoder in doctor's
tables that FFmpeg lacks gets a problem that names it, such as `FFmpeg has
no software AV1 encoder (libsvtav1)`, with the fix for FFmpeg; an FFmpeg
built without VA-API gets one problem saying so instead of one for each
VA-API encoder. A missing Opus or PCM decoder, or a missing AAC, libx264,
libx265, libsvtav1, VA-API H.264 or VA-API HEVC encoder, is a warning,
and so is Vulkan Video unable to decode H.264 or HEVC on a discrete GPU
even where VA-API can: there VA-API's frames are copied out of video
memory, which drops frames at 5.7K and up. A
missing `av1` decoder (FFmpeg's own, which VA-API and Vulkan Video use),
VA-API AV1 encoder or ProRes encoder is a note. Warnings, such as no GPU
or a Vulkan device that runs on the CPU, exit 0. Doctor tells a driver
that refuses a format (`cannot decode HEVC Main 10`) from one that decodes
it wrongly (`decodes HEVC Main wrongly (N of M samples differ from
FFmpeg's software decoder)`); for the second, the fix is a newer driver.
In `--json`, a failed check's `fault` is `ffmpeg`, `driver` or
`wrong_output`. Doctor never installs or changes anything; on Fedora it
makes one read-only `rpm -q` query to word its fixes.

`--file <FILE>` also checks a camera's file: whether it can be stitched
(its calibration is there), whether both lenses decode as `spherecut
frame --decoder auto` decodes them (VA-API on the render node, else
software), and what is missing when not. A file that can't be probed
fails doctor with probe's code for it.

### The viewer

```sh
spherecut                                  # the viewer, with its Files pane to open one
spherecut clip.mp4                         # clip.mp4 shown
spherecut clip.mp4 --cache-mb 4096         # a 4 GiB frame cache (default: a quarter of free memory, 512 MiB to 8 GiB)
spherecut clip.mp4 --adapter cpu           # render on lavapipe (or part of a GPU's name, or its PCI bus id)
```

Drag the picture to look around; with Ctrl held the drag is free to
roll. Scroll (or Ctrl+scroll) to zoom about the pointer, or + and − to
zoom about the centre.
Touchpad pinch doesn't reach applications under winit on Linux yet, so
two-finger scrolling zooms. 1, 2 and 3 switch to rectilinear, fisheye
and tiny planet, and R looks forward and level again. Space or K plays
and pauses, Right and Left (or . and ,) step a frame, Shift with them
jumps a second, Home and End go to the first and last frames, and
dragging the bar under the picture scrubs. F3 shows the overlay: the
decode path, each stage's time on the CPU and the GPU, frames shown
and dropped, the A/V offset, the cache, the slots, memory and the
audio output. File ▸ Settings changes the cache's budget, the decoder,
the filter, the GPU slots and the A/V offset at once, and keeps them;
`--cache-mb` and `SPHERECUT_FRAME_CACHE_MIB` win over the setting. The
Files pane opens videos (there is no file dialog, and files can't be
dropped on the window under Wayland). The Info pane shows the file's
probe report, and the Doctor pane the startup check's whole report; its
errors and warnings also show as banners above the viewer, each with its
fix and a Copy button. `SPHERECUT_FRAME_CACHE_MIB` and
`SPHERECUT_ADAPTER` set the cache and the GPU as the options do.

Opening an equirect video makes an untitled project of it, and the
viewer shows the output's frame (16:9 at first) with the picture past
it dimmed. Each frame shows the keys' view. Moving the view while
paused on a key's frame, or while a lane has only its first key,
edits that key; elsewhere, or while playing, it is a look-around,
marked "Not keyed", until the next play, seek or step. Alt+K keys the
view at the playhead. I and O set in and out, M drops a marker, Alt+[
and Alt+] go to the previous and next key, Ctrl+L loops in to out, and
play stops at out. The strip under the picture shows the markers, the
trim's handles and a lane of keys for each of orientation, FOV and
projection: click a key to select it and go there, drag it to move it
(Shift snaps to markers and the playhead), right-click it for its ease,
Delete removes it. Ctrl+S and Ctrl+Shift+S save the project, the Files
pane opens `.spherecut` files, Ctrl+Z and Ctrl+Shift+Z undo and redo,
and closing or opening with unsaved changes asks first.

The Curves tab draws yaw, pitch, roll, FOV and the projection over
time: drag a key's dot up or down to change that one value, Ctrl and
the wheel zoom, the wheel or a drag of the background pans. The Sphere
tab shows the whole frame with the view's outline, its path over the
trim, the keys and the look-at targets; click it to look there (for a
360 output, to turn its front there). The Inspector tab shows the view,
the selected keys, the trim, the output and the targets in numbers to
edit, and adds a target where the view looks. View ▸ Preview the 360
output shows a 360 output as it will export. File ▸ Export… (Ctrl+E)
sets the output, codec, quality, encoder, audio, motion blur, the range
(the trim or the selected keys) and the file name, in the Files pane's
folder, and exports in the background with its progress and Cancel;
each choice is kept with the project. While the picture plays or a drag
runs, the export slows to keep the window smooth.

### Rendering a frame

```sh
spherecut frame clip.mp4 -o out.png                       # frame 0, 1920x1080, rectilinear 100°
spherecut frame clip.mp4 -o out.png --time 1:02.5 --yaw 90 --pitch -20 --roll 5
spherecut frame clip.mp4 -o out.png --frame 370 --projection fisheye --fov 360 --size 1024x1024
spherecut frame clip.mp4 -o out.png --time 30000/1001 --projection stereographic --fov 300 --pitch -90   # a tiny planet
spherecut frame clip.mp4 -o out.png --morph-to stereographic --morph 0.5 --fov 150
spherecut frame clip.mp4 -o out.png --overwrite --depth 16 --json
spherecut frame two-views.mkv -o out.png --stream 2 --equirect   # the video stream FFmpeg numbers 2, rendered as an equirect
spherecut frame --project trip.spherecut -o out.png --time 2.5   # the sequence at 2.5 s, as render writes it
spherecut frame clip.mp4 -o out.png --projection equirect --yaw 90 --size 2048x1024   # the whole sphere, turned
spherecut frame VID_20261022_165829_00_008.insv -o out.png --reference --projection equirect --size 2048x1024 --time 1   # an X5 file, stitched
spherecut frame VID_20261022_165829_00_008.insv -o out.png --reference --seam weights --projection equirect --size 2048x1024   # where each lens counts
spherecut frame VID_20261022_165829_00_008.insv -o out.png --lens 1 --size 1024x1024   # lens 1 as it was coded
```

- **When**: `--time` takes seconds (`12`, `12.5`), `m:s` or `h:m:s`
  (`1:02.5`, `01:02:03.250`), a rational (`30000/1001`) or a frame
  number (`370f`), all read exactly, in clip time: 0 is the first frame
  shown, whatever the container's start time or edit list. The frame
  shown is the one on screen at that time. `--frame N` is `--time Nf`,
  counting from 0; on a variable-frame-rate file it counts frames from
  the start. A time past the last frame fails, naming the clip's range,
  and so does a frame number past it. A frame the file has lost, cut
  short or with frames missing, fails as damage rather than showing
  another frame in its place.
- **The camera**: `--yaw` (degrees right), `--pitch` (up) and `--roll`
  (clockwise, seen from behind the camera), all 0 by default;
  `--fov`, horizontal, at most 170 rectilinear, 360 fisheye and 340
  stereographic; `--projection equirect` is the whole sphere, turned,
  and takes no `--fov` or morph; `--size WxH`, 16 to 16384 a side and
  within the GPU's limit; `--depth 8|16` for the PNG.
- **The file** must be tagged as an equirect (Spherical Video V1 or V2
  metadata in an MP4 or MOV file, its projection in a Matroska or WebM
  file); `--equirect` renders an untagged one as an equirect anyway. `--stream
  N` picks a video stream by FFmpeg's index; the first is the default.
  Stereo files render the left eye, a cropped picture shows black where
  it doesn't cover the sphere, and a pose in the metadata isn't applied
  yet; each says so in a warning.
- **A camera's two lenses** (an Insta360 `.insv` master): `--reference`
  stitches them on the CPU with the file's calibration and the
  geometry Spherecut's table of cameras gives the model, which for the
  X5 is a placeholder until M3.6 fits it, so seams may show; without
  `--reference` the run is refused until M3.7 brings the GPU's stitch,
  and until then the pair decodes in software (`--decoder vaapi` or
  `vulkan` is refused).
  `--geometry binding=reversed,canvas=b,...` changes the table's
  geometry key by key (binding, canvas, turn, tz, upright,
  generation), `--stitch-distance 1.5m` (or `inf`; 2 m by default) sets
  where the lenses meet, and `--seam weights|difference` shows where
  each lens counts and how far they disagree. `--lens 0|1` shows one
  lens's picture as it was coded, on the GPU too, with no calibration
  needed. The JSON's `stitch` says what the stitch used.
- **The GPU**: `--adapter auto` (the default, or `$SPHERECUT_ADAPTER`)
  takes a GPU and falls back to lavapipe; `cpu` is lavapipe, and other
  text picks an adapter by part of its name or its PCI bus id.
  `--reference` renders with the slow f64 CPU reference instead.
  `--filter aniso` is sharper where the view squeezes the source, and
  the driver's own; `iso`, the default, is the same on every driver.
- **Decoding**: `--decoder auto` (the default) decodes with VA-API on the
  render node of the GPU that renders, and in software when that can't
  (no render node, a stream the GPU turns down, a failure), and says
  which; `software`, `vaapi` and `vulkan` force one, and the hardware
  ones fail instead of falling back (DECISIONS D39). `--render-node`
  names the node. `--zero-copy` keeps the frame on the device: VA-API
  exports it as DMA-BUFs and the GPU imports them, with no copy through
  the CPU; it fails (exit 3, `import_failed`) when the GPU, its driver
  or the decoder can't, and the JSON's `decode.transfer` says which way
  a frame came (slice 5b, DECISIONS D25). The viewer keeps Vulkan
  Video's frames on the GPU by default, decoding on its own device where
  the GPU lets it be shared; `SPHERECUT_ZERO_COPY=1` imports VA-API's
  too, `SPHERECUT_ZERO_COPY=0` copies every frame, and whatever can't
  stay on the GPU is copied.
- **The output**: something already at `-o` is refused unless
  `--overwrite` is given, and so is a link there; with `--overwrite` a
  regular file is replaced, through a link too. The PNG is written next
  to the output and linked into place whole (with `--overwrite`, renamed
  into place), so a failed or interrupted run leaves nothing behind, and
  a file that appears at the output while the frame renders is kept
  (DECISIONS D16).
- **A project's frame**: `--project P` renders the frame of P's
  sequence at `--time` or `--frame`, counted from the sequence's start,
  as `spherecut render` renders it before the encode: the keyed view,
  the output size and the motion blur are the project's, so the camera
  flags, `--size` and `--filter` don't go with it (DECISIONS D51). A
  project of a camera's two lenses is refused (`not_yet`) until M3.11
  renders projects stitched, as `spherecut render` refuses it.
- **Ctrl+C** ends the run with exit 4 and no output. It stops a seek or
  a read at once; during probe, the GPU's set-up, the render or the
  PNG's encode it takes effect when that step ends, which at a large
  `--size` can take seconds. A second Ctrl+C kills it at once. Started
  with SIGINT ignored, as a script's background job (`cmd &`) is, frame
  leaves it ignored.

On success it prints one line: `wrote out.png: 1920×1080 rectilinear
100° at 12.5 s (frame 375, pts 375000), decoded on VA-API, rendered on AMD
Radeon 8060S (RADV)`. It exits 0 when the PNG is written; 1 when the
file, the frame or the output is the problem (missing, damaged before
the frame, not an equirect, past the end, the output in the way); 2 for
a command line that doesn't parse or a value out of range; 3 when a
device or codec it needs is missing (no Vulkan driver, no adapter by
that name, no decoder for the codec, or the hardware `--decoder` named
not working, which `spherecut doctor` explains); and 4 when interrupted.

### Rendering a project

```sh
spherecut render trip.spherecut -o trip.mp4                     # with the export settings the project keeps
spherecut render trip.spherecut -o trip.mp4 --preset flat-9x16-1080 --codec hevc --quality small
spherecut render trip.spherecut -o trip.mp4 --range 1:00..90f --draft   # part of it, at half the size, fast
spherecut render trip.spherecut -o trip360.mp4 --preset 360-5760 --encoder software --overwrite
spherecut render trip.spherecut -o trip.mp4 --json              # a JSON line a second, the result last
```

`spherecut render` exports a project's sequence to an MP4 with the
export settings the project keeps (File ▸ Export… sets them, DECISIONS
D51); each flag changes one for this run only, and isn't saved:

- `--preset`: `flat-16x9-1080` and the rest of `flat-WxH-SIDE` (16x9,
  9x16, 1x1 or 4x5 at 1080, 1440 or 2160), or `360-3840`, `360-5760` or
  `360-7680`. `--draft` renders at half each side, rounded up to a
  multiple of 8, with the encoder's fastest settings.
- `--codec` (`h264`, `hevc`, `av1`), `--quality` (`high`, `medium`,
  `small`), `--audio` (`copy`, `aac`, `none`), `--motion-blur`, and
  `--eight-bit` for 8-bit from a 10-bit source.
- `--encoder`: `auto` encodes on the GPU's VA-API encoder and goes on in
  software, saying why, when it can't open or fails in its first second;
  `vaapi` fails instead (exit 3), as `auto` does when VA-API is the only
  encoder for the codec (H.264 with stock Fedora's ffmpeg-free, which
  has no libx264), and `software` never tries the GPU.
- `--range IN..OUT`: the part of the sequence, in sequence time, each
  end as frame's `--time` takes it and either left out (`2..7.5`,
  `1:00..90f`, `5..`, `..10`).
- `--decoder` (`auto`: VA-API on the rendering GPU's node, going on in
  software; or `software`), `--adapter` (as frame's, or
  `$SPHERECUT_ADAPTER`) and `--overwrite`.

The file is written beside the output and checked before it is put in
place in one rename: it opens, its `moov` comes before the media, its
codec, size, frames, duration and rate are the ones asked for, a 360
export has its `st3d`, `sv3d` and V1 metadata and a flat one none, the
audio is right, and FFmpeg's own demuxer reads the header back as the
same sphere, or none, with no complaint in its log. So Ctrl+C, a failure
or a full disk leaves nothing at `-o`. A line on stderr says what is
rendering, then one a second how far it has got, and stdout gets one
line at the end: `wrote trip.mp4: 600 frames (20 s) at 3840×1920, HEVC
10-bit in hevc_vaapi, aac audio, 45.1 MB, 13 checks passed`. The exit
statuses are frame's: 1 when the project, the output, the export or a
check of the file is the problem, 2 for a command line that doesn't
parse, 3 when a GPU, an adapter or an encoder it needs is missing or
fails, and 4 when interrupted.

A project of a camera's two lenses (an `.insv` an agent's `project
create` made, which stores the file's calibration and the lens
geometry it was made with: DECISIONS D68) isn't exported yet: until
M3.11 stitches a project's export, `render` refuses it with exit 1
(`not_yet`) and writes nothing, rather than export one lens as an
equirect; `frame --reference` of the `.insv` itself shows it
stitched, and an EAC pair has no stitched view yet. When the project's
geometry was a provisional guess and Spherecut's table of cameras has
it measured, or guesses differently (the table keeps no order, so the
project's guess may be the later one), `render` warns
(`provisional_geometry`), giving the table's evidence: the project
keeps its own geometry, and renders the same, until it is changed.

### Agents

```sh
spherecut mcp --read-root ~/Videos            # the MCP server an agent starts: it reads only under ~/Videos
spherecut mcp --read-root ~/Videos --allow-write --write-root ~/Videos/edits   # and makes, edits and renders projects there
spherecut call probe '{"files": ["clip.mp4"]}'                         # one tool, from a shell
spherecut call look '{"file": "clip.mp4", "times": ["12.5"], "view": {"yaw": 90}}' --image-dir /tmp
spherecut agents install-skill                # the recipes agents follow with the tools
```

`spherecut mcp` serves eleven tools to an AI agent over MCP on stdin
and stdout: probe, look, analyze, check and doctor read footage;
project reads a project and, with a write root, makes one; edit and
undo change it and render and job export it in the background, listed
only with a write root; and compare measures how alike a video is to
another or to a project's render. [docs/agents.md](docs/agents.md) has each
client's setup line, every tool and its limits. `spherecut call` runs
one tool with the same roots and limits and prints a
`spherecut.call/1` document; it runs all but render and job, whose
exports end with their server and so run only under `spherecut mcp`
(`spherecut render` is the same export). `spherecut agents
install-skill` installs the skill, the recipes agents follow, for
Claude Code, Codex, Gemini CLI and Copilot CLI (`--dry-run` says what it
would do).

### JSON output

`spherecut probe --json`, `spherecut doctor --json` and `spherecut frame
--json` print exactly one line of JSON on stdout per run, on success and
on failure, and keep stderr for the log. `spherecut render --json`
prints one envelope a line, the last of them the result or the failure,
and `spherecut call` always prints one `spherecut.call/1` document.
[docs/cli-json.md](docs/cli-json.md) documents it
field by field, with the codes, the exit statuses and an example per
command; DECISIONS D27 says why it is this way. In short:

```json
{"schema":"spherecut.probe/1","ok":false,"result":{"files":[...]},"error":{"code":"files_failed","message":"..."},"warnings":[]}
```

- `schema` names the document and its version. Within a version, fields
  are only ever added.
- probe's `result` is `{"files": [...]}`, for one file as for many: each
  has its `path`, `ok`, its `report` whenever FFmpeg could open it, and
  its `error` when it fails. A damaged file has both.
- An error is `{"code", "message", ...}`; the code (`truncated`,
  `not_found`, `not_media`, `usage`, ...) is stable, the message is for
  people. The report's `damage` and `warnings` are `{"code", "message"}`
  too.
- Times are exact: `{"exact": "1001/30000", "seconds": 0.0333...}`, where
  `exact` is the time and `seconds` only for display. Frame rates and time
  bases are `"30000/1001"`, or `"25"`.
- frame's `result` says which frame it rendered (`pts` and its time,
  with its number where it is known), what was asked for, the view, how
  it was decoded (and why it fell back, if it did), the GPU, the colour
  it decoded with and what of it was assumed, and the source stream; a
  failed run has no `result`. Its `warnings` say what it did with the
  file's spherical metadata.
- Exit statuses: 0 when the run did what was asked, 1 when an input or a
  check failed, 2 for a command line that doesn't parse (also a document
  under `--json`), 3 when `frame`, `render` or `call` lacks a codec or
  device it needs, and 4 when one of them was interrupted (code
  `cancelled`). A panic is an `internal` error, exit 1; an empty stdout
  means a signal ended the run: Ctrl+C for probe and doctor, a second
  Ctrl+C for frame, render and call, or a crash.

### Logging

Logs go to stderr. The default filter is `warn,ffmpeg=off`: Spherecut's
warnings, and none of FFmpeg's own messages, since doctor's device checks
make FFmpeg log errors that are expected. `SPHERECUT_LOG` adds `tracing`
EnvFilter directives to that default:

- `SPHERECUT_LOG=spherecut_media=debug` shows Spherecut's debug events,
  still without FFmpeg's messages.
- FFmpeg's messages appear, under the target `ffmpeg`, when the value names
  that target (`ffmpeg=warn`, `debug,ffmpeg=debug`) or when its last global
  level is more verbose than `warn` (`info`, `debug`, `trace`). `warn` and
  `error` keep them off, and `off` turns off everything. FFmpeg's verbose
  level shows as debug, its debug and trace levels as trace. Each event is
  one whole line of FFmpeg's log, starting with the context that logged
  it, such as `[hevc @ 0x...]`.
- A value that doesn't parse is reported (`warning: ignoring
  SPHERECUT_LOG=...`) and the default applies.

The filter doesn't change probe's reports: their `ffmpeg` lines list
FFmpeg's warnings and errors about each file whatever it is set to.

### Environment

- `SPHERECUT_LOG`: the log filter (above).
- `SPHERECUT_ADAPTER`: the GPU to render on, as `--adapter` takes it,
  for the viewer, `frame` and `render`.
- `SPHERECUT_FRAME_CACHE_MIB`: the viewer's frame cache, as
  `--cache-mb`.
- `SPHERECUT_ZERO_COPY`: `1` keeps VA-API's frames on the GPU too, `0`
  copies every frame (Rendering a frame, Decoding).
- `SPHERECUT_VAAPI_DRAIN`: a diagnostic of the copy path's stall on AMD
  GPUs, until a run on odin picks the default. With `download`, a
  VA-API decoder that copies its frames to memory first waits for every
  picture queued on the GPU, so that freeing the copy's staging buffer
  stalls nothing else; with `picture` it waits after every packet too,
  zero copy's decoders included. `off` or `0`, or unset, the default,
  waits for nothing. The viewer and the playback benchmarks read it.
- `SPHERECUT_GATE_PROBE`: another diagnostic of that stall, off by
  default. Set to a whole number of milliseconds from 2 to 50, a
  `--smoke` run submits a tiny job on the viewer's GPU queue that often
  while it plays, and gives in its document the percentiles of the
  jobs' latencies and each job slower than 8 ms (`result.probe` in
  `docs/cli-json.md`); anything else is ignored with a warning.
- `SPHERECUT_EXPORT_DEVICE`: where the window's exports render. By
  default on a device of their own on the window's GPU (M2.10,
  DECISIONS D56); `window` puts them on the viewer's own device, as
  M2.7 ran them.

For development and the tests:

- `SPHERECUT_FFMPEG_COMPAT` and `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`:
  the FFmpeg builds `cargo xtask check` also builds against, and where
  (Development).
- `SPHERECUT_UPDATE_GOLDENS=1` blesses the golden images (DECISIONS
  D32). `SPHERECUT_BLESS_PROJECT=1` rewrites the checked-in project
  file `crates/core/tests/data/schema2.spherecut`, the current
  schema's fixture, whose test fails when a save no longer writes it.
  It never rewrites `schema1.spherecut`, which loads unchanged forever:
  a new schema gets a migration and a fixture of its own (DECISIONS
  D47).
- `SPHERECUT_TIER2`, `SPHERECUT_SAMPLES`, `SPHERECUT_PERF_MEDIA` and
  `SPHERECUT_GUI_OUT`: what `cargo xtask hw-test` hands the Tier 2
  tests (run them, the samples folder, the perf clips' folder, where
  the GUI runs' documents go). `SPHERECUT_RENDER_NODE` names the render
  node they test. A Tier 2c test needs only `SPHERECUT_SAMPLES`
  (Development).
- `SPHERECUT_BENCH_RUNS`, `SPHERECUT_BENCH_OUT` and
  `SPHERECUT_PLAYBACK_TIMELINE`: how often a benchmark repeats, where
  its numbers go, and a folder for the playback benchmarks' timelines
  (`docs/perf.md`).
- `SPHERECUT_DEBUG_PANIC` and `SPHERECUT_DEBUG_FRAME_RACE` work in
  debug builds only, for the contract tests' panics and frame's output
  race. `SPHERECUT_FOLDER_CHILD`, `SPHERECUT_FOLDER_DIR`,
  `SPHERECUT_PAIR_DECODE_RUN` and `SPHERECUT_TEST_HOLD_TEMP_IN` are how
  a test tells a copy of its own binary, run as a child, what to do.

## Development

```sh
cargo xtask check        # rustfmt, clippy and rustdoc -D warnings, the MSRV build, Tier 1 tests, doc tests, cargo-deny, cargo-audit
cargo xtask check --fix  # same, applying rustfmt
cargo xtask gen-media    # synthetic test clips into test-media/ (--quick skips the 4K clip, --out <dir>)
cargo xtask hw-test      # Tier 2 run on real hardware; report in hw-reports/
cargo xtask hw-test --list   # the Tier 2 tests it would run
cargo xtask compat PREFIX... [--test PREFIX] [--clean]   # clippy and a link against other FFmpeg builds, Tier 1 against one
```

`cargo xtask compat` takes FFmpeg install prefixes, such as those
`scripts/build-ffmpeg.sh TAG DIR` builds from a release tag, or the
colon-separated list in `SPHERECUT_FFMPEG_COMPAT`, which `cargo xtask
check` also runs it on (DECISIONS D2). It builds against each in a target
directory of its own, `spherecut-compat-<label>`, in the directory
`SPHERECUT_FFMPEG_COMPAT_TARGET_DIR` names or else in `compat/` in
cargo's target directory, and deletes only directories it made. A
build passes only if every FFmpeg library it bound came from the
prefix, checked before the build and after it.

`cargo xtask check` builds the workspace with the toolchain of
`rust-version` (1.95) through rustup, and skips that step, saying so,
when rustup or the toolchain is missing (`rustup toolchain install 1.95
--profile minimal`); under CI it fails instead. Clippy's network lints,
rustdoc, the MSRV build and cargo-deny's bans each also run on a scratch
crate made to fail them, and a check whose control passes fails
(DECISIONS D8, D32). It needs `cargo install --locked cargo-deny cargo-audit`
once (or `--no-supply-chain` to skip those two), strace, and ALSA's
development files: it stops first if `strace -V` fails or pkg-config
finds no `alsa`, with the command that installs it (`sudo dnf install
strace alsa-lib-devel`, `sudo pacman -S strace alsa-lib` or `sudo apt
install strace libasound2-dev`).

CI (`.github/workflows/check.yml`, DECISIONS D78) runs `cargo xtask
check` on GitHub's Ubuntu 24.04 runner, with the Ubuntu packages above
and the MSRV build required, and `cargo xtask compat` against FFmpeg
8.1.3 and 9.0.2 built by `scripts/build-ffmpeg.sh`. It runs only when
started by hand (Actions, check, Run workflow) until Pat turns its push
trigger on, and it can't run Tier 2, the benchmarks or Tier 1 on a real
GPU, so the full check, compat and property-test runs before a push
stay the rule.

Tests come in three kinds. Tier 1 runs anywhere, including machines without
a GPU. It needs the FFmpeg codecs listed above and Mesa's lavapipe Vulkan
driver (`mesa-vulkan-drivers` on Fedora and Ubuntu, `vulkan-swrast` on
Arch): `crates/cli/tests/doctor.rs` runs doctor with the Vulkan loader
limited to lavapipe (`VK_DRIVER_FILES`, `VK_ICD_FILENAMES`), and
`crates/media/tests/hw_refusal.rs` checks that the decoder turns streams
down on lavapipe's Vulkan device instead of decoding them in software.
`crates/cli/tests/confinement.rs`, `doctor.rs` and `mcp.rs` run probe,
doctor, frame, render and the MCP server under `strace -f` and fail on
any `AF_INET` or `AF_INET6` socket or a `connect` to anything but a
Unix socket, or on probe opening a file a playlist lists; without
strace they fail rather than skip. `crates/cli/tests/frame.rs` renders
on lavapipe, with its own golden images in `crates/cli/tests/goldens/`
(`SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test frame
pipeline_goldens` blesses them, as for render's, DECISIONS D32). The
tests generate the media they need; `gen-media` is not needed first.

Tier 2 needs a real GPU or camera samples; a plain `cargo test` lists
those tests as ignored, with what each needs. Tier 2h tests need the
hardware, real time or the GUI, and run only under `cargo xtask
hw-test`, which sets `SPHERECUT_TIER2`; without it they skip
(`require_hw_run`). Tier 2c tests need only Pat's camera samples and
the CPU, so they run anywhere the samples are, with
`SPHERECUT_SAMPLES` naming the folder (`require_samples_cpu`):

```sh
SPHERECUT_SAMPLES=samples cargo test -p spherecut-media --test tier2_tracks -- --ignored samples_x5_tracks_equal
```

A Tier 2 test whose prerequisite is missing fails with a `TIER2-SKIP`
marker rather than passing, and every check has a control that must
fail (`spherecut_testkit::control::must_fail`, DECISIONS D60).

`cargo xtask hw-test` runs every Tier 2 test, 2h and 2c. It needs no
sudo. It records the environment (the power settings and every render
node among it), lists missing prerequisites with the command that
installs them, builds the workspace in release mode, makes the perf clips
the playback benchmarks play, runs every Tier 2 test and benchmark, then
runs Tier 1 on the machine, and writes one report to
`hw-reports/<host>-<commit>.md` (the commit gets `-dirty` when the checkout
has changes outside `hw-reports/`). The perf clips, synth's noisy 5.7K
and 8K HEVC at a camera's bitrate, take x265 a while the first time
(27 minutes for one 5.7K clip on 4 CPUs, the 8K one longer); they are kept in `hw-media/` in cargo's target directory and made
again only when synth's spec of them changes (DECISIONS D37). Options:

- `--samples <dir>`: camera samples (default `samples/` in the checkout; a
  relative path is taken from the current directory). The path reaches the
  tests byte for byte, as `SPHERECUT_BENCH_OUT` (see `docs/perf.md`) does,
  and need not be valid UTF-8.
- `--quick`: a debug build, with the benchmarks, and so the perf clips,
  left out.
- `--no-tier1`: leave out the Tier 1 run on the machine.
- `--only <name>`: run only the Tier 2 tests whose names, as `--list`
  prints them, contain `<name>` (repeatable); the report lists the rest as
  left out. `cargo xtask hw-test --only gui_smoke --no-tier1` runs the GUI
  smoke run again in a few minutes, and `--only gui_decode_behind` the
  runs with decode held back.
- `--list`: print the Tier 2 tests, as the report names them, and stop.
- `--out <file>`: where to write the report.

Tier 2 tests use `/dev/dri/renderD128`, or `SPHERECUT_RENDER_NODE`.
`gui_smoke` opens the viewer on each perf clip and sample, plays it with
the overlay on and judges what it measured; it needs a desktop session
(GNOME or Hyprland, unlocked and on screen), which it finds from a
terminal inside it or from an SSH shell, and skips without one. It
fails at once if logind says the screen is locked, and a window left
undrawn ends after 15 s. On GNOME, `gnome-session-inhibit --inhibit
idle:suspend cargo xtask hw-test ...` keeps the screen from locking
while it runs. Its screenshots go next to the report.
`audio_output_device_plays` plays a quiet 440 Hz tone for about 2 s on
the default output device.
Benchmarks repeat `SPHERECUT_BENCH_RUNS` times (default 3); see
`docs/perf.md`.

`cargo xtask hw-test` exits 1 when a build prerequisite is missing, the
build fails, listing the Tier 2 tests fails or finds none, a Tier 2 test
fails or does not run, a test binary dies, even after its tests
reported, a perf clip can't be made, or a Tier 1 test fails on the
machine. Skips alone exit 0. The report is written either way. Your cargo
settings `term.verbose`, `term.quiet`, `term.color` and
`term.progress.when` (in config or as `CARGO_TERM_*`) and
`RUST_TEST_NOCAPTURE` don't change how it reads cargo's output.

Camera samples go in `samples/<camera>/` (git-ignored); `docs/SAMPLES.md`
lists what is useful.

Design and decisions: `ARCHITECTURE.md`, `DECISIONS.md`. Camera format
notes: `docs/formats/`. Benchmark numbers: `docs/perf.md`.

## License

GPL-3.0-or-later. See `LICENSE`.
