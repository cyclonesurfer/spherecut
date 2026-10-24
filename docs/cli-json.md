# The CLI's JSON contract

What `spherecut probe --json`, `spherecut doctor --json` and `spherecut
frame --json` print, for scripts and AI agents. A1's MCP tools return the
same time shapes and codes. DECISIONS D27 records why it is this way.

## One document per run

With `--json`:

- **stdout** carries exactly one JSON document per run, on success and
  on failure: one line of compact JSON, ending in a newline. Newlines
  inside strings are escaped, so the document is always one line.
- **stderr** is for people: the log (see `SPHERECUT_LOG` in the README)
  and an `error:` line for each failure. Its wording changes; never
  parse it.
- **The exit status** says how the run ended (below), and so do the
  document's `ok` and `error`.
- `--help` and `--version` print text, with `--json` too, and exit 0.
- When the reader of stdout goes away (`spherecut probe --json *.mp4 |
  head -c 0`), the document isn't written and the exit status is the
  same as it would have been. When stdout can't be written at all (a
  full disk), stderr says so and the run exits 1.
- A panic, a bug in Spherecut on any thread, is still one document: an
  `internal` error that says where it panicked, with exit 1. Rust's own
  report of it goes to stderr.
- If stdout holds no document at all, the run was killed by a signal
  before it wrote one; a shell shows 128 + the signal's number as `$?`.
  Treat that as a failed run. A kill on purpose, such as SIGINT (Ctrl+C)
  or SIGTERM (130 or 143), is no bug: probe and doctor don't catch them,
  and `frame` catches only the first Ctrl+C, which ends it with a
  `cancelled` document and exit 4 (below); a second Ctrl+C kills it.
  Anything else is a crash, such as SIGSEGV or SIGABRT inside FFmpeg (139
  or 134): please report it.

## Versions

`schema` names the document and its version: `spherecut.probe/1`,
`spherecut.doctor/1`, `spherecut.frame/1`, and `spherecut/1` for a usage
error that names no subcommand. Check the whole string.

Within a version:

- fields are only added, never removed, renamed or given another type;
- a code keeps its name and meaning;
- new codes, and new values of fields that list choices (such as a
  stream's `kind` or a camera's `layout`), may appear.

So a reader ignores fields it doesn't know, and handles a code it
doesn't know by the exit status. Anything else is a new version, such as
`spherecut.probe/2`. The order of keys in an object means nothing.

## The envelope

```json
{"schema":"spherecut.probe/1","ok":true,"result":{...},"warnings":[]}
```

| Field | Type | What |
|---|---|---|
| `schema` | string | The document's name and version |
| `ok` | bool | Whether the run did what was asked: true exactly when there is no `error` |
| `result` | object, or absent | The command's result (below). Absent for a `usage` or `internal` error. probe and doctor have one in every other run, failed or not: a damaged file still has its report, and doctor's errors come with doctor's report. `frame` has one exactly when it wrote the frame: a failed `frame` run has none |
| `error` | error, or absent | Why the run failed. Absent when `ok` is true |
| `warnings` | array of warnings | About the run as a whole. probe and doctor report what they find in `result`, so for them this is always empty in version 1. `frame`'s are about its file and its devices, and a failed run keeps the ones it gave before it failed |

An **error** is:

| Field | Type | What |
|---|---|---|
| `code` | string | Stable, snake_case (see Codes) |
| `message` | string | For people; the wording may change |
| `path` | string, or absent | The file it is about |
| `ffmpeg_log` | array of strings, or absent | FFmpeg's warnings and errors about a file it could not open, which often say why (`moov atom not found`). Absent when empty |

A **warning**, and each of probe's problems, is `{"code", "message"}`,
both strings.

Paths are text: bytes of a file name that aren't UTF-8 show as U+FFFD,
as in the text output.

## Times and rationals

Time in Spherecut is exact, and so is its JSON.

- A **rational** (a frame rate, a time base, a sample aspect ratio) is a
  string: `"n/d"`, reduced, with `d` > 1, or `"n"` when it is an integer.
  `n` may be negative. 29.97 fps is `"30000/1001"`, 25 fps is `"25"`, a
  time base is `"1/90000"`.
- A **time** is an object:

  ```json
  {"exact": "7007/30000", "seconds": 0.23356666666666667}
  ```

  `exact` is the time in seconds as a rational, and is the time.
  `seconds` is the same value as a float, for display and rough sums;
  never use it to tell frames apart.
- A **frame time** is a time that is a frame of a video stream, in clip
  time (below). It adds `frame`, the frame's index from the stream's
  first presented frame (0), when the index is known: for a
  constant-frame-rate stream, or a frame reached by counting from the
  start. A variable-frame-rate stream's frames are addressed by time
  alone, so `frame` may be absent, and a reader must allow for that; a
  frame time without it is a plain time:

  ```json
  {"exact": "1001/10000", "seconds": 0.1001, "frame": 3}
  {"exact": "1001/10000", "seconds": 0.1001}
  ```

  probe and doctor have none. `frame` gives one for the frame it
  rendered (`result.frame.time`), and A1's tools give them wherever a
  time is a frame.

Two timelines, which differ for many files:

- **The container's**, in probe: a container's `start_time` and a
  stream's `start_pts` are FFmpeg's timestamps as the file has them, and
  `start_time` may be far from 0 (a file made with `ffmpeg
  -output_ts_offset 10` has one near 10 s, and its video stream's
  `start_pts` is 128000 at `time_base` `"1/12800"`). `duration`s are
  lengths, not positions.
- **Clip time**, in `frame` and A1's tools: 0 is the stream's first
  presented frame, whatever the container's `start_time`, edit list or
  B-frame delay (M1 plan, section 3.3). `--time 0.5` is half a second
  into the clip, so the first frame of the file above is `frame --time
  0`, not `--time 10`. A frame time is in clip time: frame n of a
  constant-rate stream is at n / `avg_frame_rate`.

To read `exact`: split at `/` and parse both parts as 64-bit integers
(`-?[0-9]+` and `[0-9]+`), never as floats. Spherecut reads its own
output back to the same value; `spherecut_core::Rational` does it with
`str::parse`, and `Time` and `FrameTime` deserialize from these objects,
ignoring `seconds`. A `FrameTime` without `frame`, or with `"frame":
null`, has `frame: None`; Spherecut leaves the field out rather than
writing null.

Integer timestamps stay integers: a stream's `start_pts` is in ticks of
its `time_base`.

## Exit statuses

| Status | Meaning | probe | doctor | frame |
|---:|---|---|---|---|
| 0 | The run did what was asked | Every file is ok (warnings don't count) | No problem of severity error | The PNG is written (warnings don't count) |
| 1 | An input or a check failed, or Spherecut itself did (`internal`) | At least one file failed | At least one problem is an error: no H.264, HEVC, ProRes or AAC decoder, or nothing that decodes AV1 (D18) | No frame: the output is in the way, the file is missing, damaged before the frame or not an equirect, the frame is past the end, the GPU failed, ... (Codes) |
| 2 | Usage error: the command line doesn't parse | yes | yes | Also a value clap can't check alone: a `--fov` past the projection's limit, a `--size` past the GPU's, a `--stream` that isn't a video stream, `--reference` with `--filter aniso` |
| 3 | A codec or device the command itself needs is missing | never | never | `no_gpu`, `adapter_not_found`, `no_decoder`, and `hw_unavailable`, `hw_unsupported` or `hw_failed` when `--decoder vaapi` or `vulkan` named the hardware |
| 4 | Interrupted (SIGINT), leaving no output file; the document's code is `cancelled` | never | never | The first Ctrl+C |

The GUI is to exit 3 too, without a display (`no_display`, M1's last
slice). A missing decoder is a warning in probe and a finding in doctor,
so neither exits 3: doctor checks capability and reports it. frame's
`--decoder auto` never exits 3 for the hardware: it decodes in software
instead, and says why in `result.decode.fallback`.

## Codes

Every code the contract defines. A file's code fails the run with
`files_failed` and exit 1.

The run's `error.code`:

| Code | Exit | Meaning |
|---|---:|---|
| `usage` | 2 | The command line doesn't parse; the message is clap's, with the usage line |
| `files_failed` | 1 | probe: at least one file failed; each file's `error` says why. The message names the first |
| `doctor_errors` | 1 | doctor: at least one problem is of severity error; the message lists them, and `result.issues` has them in full |
| `internal` | 1 | A failure of Spherecut's own, such as a panic or a document that can't be made; the message says where, and there is no `result`. Please report it |
| `output_exists` | 1 | frame: something is at the output (a file, or a link, even a dangling one) and `--overwrite` wasn't given; or something appeared there while the frame was rendered, which is left alone |
| `not_equirect` | 1 | frame: the file isn't tagged as a stitched equirect (dual fisheye, a cubemap, an LRV proxy, a 2:1 picture without spherical metadata); the message says what it is. `--equirect` renders it as one anyway |
| `out_of_range` | 1 | frame: no frame at the time or number asked for: it is past the end. The message gives the clip's range, or its frame count |
| `cancelled` | 4 | frame: interrupted by the first Ctrl+C (SIGINT); no output file is written, and the temporary file is removed |

A file's `error.code` in probe (`result.files[].error.code`):

| Code | Has a report | Meaning |
|---|---|---|
| `not_found` | no | No such file, including a path that looks like a URL (`http://host/x.mp4` is a path) |
| `io_error` | no | Another error reading it, such as permission denied, a read error from a failing disk or card, or a path through a regular file. Never `damaged`: the file may be fine |
| `not_a_file` | no | A directory, pipe, device or socket |
| `not_media` | no | Empty, or FFmpeg doesn't know it as media |
| `unsupported_container` | no | FFmpeg reads it as a container Spherecut doesn't open (an HLS playlist, an ffconcat script, WAV, ...); the message names it (D24) |
| `nested_open_refused` | no | Opening it, FFmpeg tried to open another file or URL, which is refused (D24). A guard: with only the MP4/MOV and Matroska demuxers allowed, and MP4's references to other files off, no file is known to cause it |
| `truncated` | yes or no | It was cut short: a top-level MP4 box or the Matroska Segment runs past the end of the file, or the file ends before its Matroska Segment starts. Without a report when FFmpeg can't open what is left |
| `damaged` | yes or no | Its structure is damaged in another way (README, "Build and run", lists what probe checks). Without a report when FFmpeg can't open it |
| `no_video_or_audio` | yes | It holds only subtitles, data or attachments |
| `open_failed` | no | FFmpeg could not open it for a reason probe can't name more closely, such as a Matroska file damaged inside (probe checks only a Matroska file's length); `ffmpeg_log` often says why |

A report's `damage[].code`: each piece of damage is `truncated` or
`damaged`, as above; a file with any `truncated` damage fails as
`truncated`.

| Code | Meaning |
|---|---|
| `truncated` | The cut itself: the box or Segment that runs past the end |
| `damaged` | Anything else probe finds wrong with the structure |

A report's `warnings[].code`, which don't fail the file:

| Code | Meaning |
|---|---|
| `too_large` | A stream's pictures are larger than the 2^27 pixels (16384×8192) Spherecut decodes (D24); the message gives the size |
| `no_decoder` | This FFmpeg has no decoder for a stream's codec |
| `cannot_decode` | FFmpeg could not set a stream up (an unknown sample entry, an empty track), or its decoder lacks a feature the stream uses |
| `unexpected_first_box` | FFmpeg reads the file as MP4/MOV although its first box is of a type such files don't start with |
| `track_without_stream` | An MP4 `trak` that FFmpeg gives no stream |

frame's `error.code` is also one of probe's file codes above, for the
file it opens (`not_found`, `not_media`, `truncated`, ... with `path`
and, when FFmpeg couldn't open the file, `ffmpeg_log`), with exit 1;
`not_a_file` and `io_error` are about the output as well (a directory or
device at `-o`, a directory that doesn't exist). A seek that damage
stops short of the frame fails with the damage's cause: `truncated` (the
file is cut short before it), `damaged` (corrupt data), or `io_error` (a
read error); the message names the last good frame's time. A cut inside
the first frame's own data is `decode_failed`, since no frame decodes
at all.

Errors of the decoders, which probe and doctor don't give, and which
frame does (exit 1 unless the table says 3):

| Code | Meaning |
|---|---|
| `decode_failed` | A decoder failed, or the file has no video stream |
| `no_decoder` | This FFmpeg has no decoder for the stream's codec; the message names it, by its four-character code when FFmpeg has no name for it (exit 3) |
| `too_large` | As the warning above, as an error: the decoder refuses the stream |
| `hw_unsupported` | The hardware decoder refuses the stream; software may still decode it (exit 3 when `--decoder vaapi` or `vulkan` named it) |
| `hw_failed` | Decoding or downloading a frame on the hardware device failed: the device's failure, not damage in the file (D30); software may still decode it (exit 3 when `--decoder` named it) |
| `hw_unavailable` | The hardware device can't be opened, such as no render node or a Vulkan device that can't decode (exit 3 when `--decoder` named it) |
| `unsupported_pixel_format` | The decoder's pixel format is neither one Spherecut uploads as it is nor one swscale converts; the message names it |
| `not_implemented` | A part of the decoding backend a later M1 slice brings, such as decoding audio; the message names it |

With `--decoder auto`, the `hw_*` codes never fail the run: they are the
`code` of `result.decode.fallback`.

Errors of the GPU, which frame gives:

| Code | Exit | Meaning |
|---|---:|---|
| `no_gpu` | 3 | Vulkan lists no adapter at all: no driver, or no loader. The message names `spherecut doctor`, which gives the command that installs them |
| `adapter_not_found` | 3 | No adapter matches `--adapter` (or `SPHERECUT_ADAPTER`); the message lists those there are |
| `gpu_failed` | 1 | The adapter gave no device, the device refused work or was lost, or reading the picture back failed |
| `out_of_gpu_memory` | 1 | The GPU ran out of memory |
| `invalid_input` | 1 | A picture the renderer can't use, such as a plane that doesn't match its layout |
| `too_large` | 1 | The source picture is larger than the GPU's textures (a `--size` past them is a `usage` error) |
| `internal` | 1 | As above |

frame's `warnings[].code`, which don't fail the run:

| Code | Meaning |
|---|---|
| `stereo_left_eye` | A stereo source (Spherical Video V2 top-bottom or left-right): its left eye is rendered |
| `stereo_unknown` | A stereo layout frame doesn't know (`stereo_custom`, or a value the specification doesn't define): the whole picture is rendered, as mono |
| `crop_applied` | Spherical Video V2 crop bounds: the picture covers part of the sphere, which the message gives, and the rest is black |
| `pose_not_applied` | A non-zero Spherical Video V2 pose, which M1 doesn't apply (M2 does); the message gives it |
| `anisotropy_unavailable` | `--filter aniso` on a device without anisotropic filtering: the render is isotropic, and `result.filter` is `iso` |

`truncated` and `damaged` (and `io_error`, a read error the decoder
meets) are warnings too, for damage probe finds before the seek and for
damage the decoder meets after the frame: the frame is still exact, so
the run goes on. Damage before the frame fails
the run instead (above).

`no_display` (exit 3) is planned for the GUI, and not given yet.

`crates/cli/tests/contract.rs` lists every code, so a rename fails a
test.

## `spherecut probe --json <FILE>...`

`result` is always `{"files": [...]}`, one entry per file in the order
given, for one file as for many. The run is ok when every file is.

A file entry:

| Field | Type | What |
|---|---|---|
| `path` | string | The path as given |
| `ok` | bool | Whether the file passes: true exactly when there is no `error` |
| `report` | report, or absent | What probe found, whenever FFmpeg could open the file |
| `error` | error, or absent | Why the file fails. A damaged file has both `report` and `error`. `error.ffmpeg_log` is there only without a report; with one, FFmpeg's log is the report's `ffmpeg_log` |

A report:

| Field | Type | What |
|---|---|---|
| `path` | string | The path as given |
| `file_size` | integer | Bytes |
| `container` | object | `format` (FFmpeg's demuxer name, such as `mov,mp4,m4a,3gp,3g2,mj2`), `format_long` (its description), `duration` and `start_time` (times, or null), `bit_rate` (bits per second, or null), `tags` (an object of strings) |
| `streams` | array | One per FFmpeg stream, below |
| `mp4` | object, or null | For MP4/MOV files: `brand` (string or null), `top_level` (the first 64 top-level boxes, as `[type, size]` pairs in file order), `top_level_omitted` (how many more there are; a fragmented recording has thousands), `trailing_bytes` (after the last whole box, where some cameras put vendor data), `truncated` (the top-level box cut off by the end of the file: `fourcc` (string or null), `offset`, `header_len`, `size` (or null), `present`; or null), `warnings` (the box reader's own, as strings; each is also in `damage`) |
| `camera` | object, or null | `vendor`, `model`, `firmware` (strings or null), `layout` (one of `equirect_tagged`, `equirect_likely`, `insta360_dual_fisheye`, `go_pro_eac`, `dji_dual_fisheye`, `low_res_companion`, `unknown`), `evidence` (strings) |
| `structure` | object | How much of the structure was checked for damage: `{"check": "full"}` (MP4/MOV: every top-level box and the sample tables in `moov`, but not a fragmented file's fragments: the sample tables in its `moof` boxes (`trun`) aren't checked), `{"check": "length"}` (Matroska: only the Segment's length), or `{"check": "none", "why": string}` |
| `damage` | array of problems | Why the file looks truncated or corrupt; empty for a healthy file |
| `warnings` | array of problems | What may keep a stream from playing, without damage |
| `ffmpeg_log` | array of strings | FFmpeg's warnings and errors while reading the file, each one whole line, starting with the context that logged it (`[aac] ...`) when FFmpeg gives one; a repeated line once with a count, at most 32. Healthy files can have some |

A stream:

| Field | Type | What |
|---|---|---|
| `index` | integer | FFmpeg's stream index |
| `kind` | string | `video`, `audio`, `data`, `subtitle`, `attachment` or `unknown` |
| `codec` | string or null | FFmpeg's codec name, such as `hevc` |
| `codec_tag` | string or null | The container's four-character code, such as `hvc1`; bytes that aren't printable are `\xNN` |
| `profile` | string or null | As FFmpeg names it, such as `Main 10` |
| `level` | integer or null | As FFmpeg reads it, such as `31` for H.264 level 3.1 |
| `time_base` | rational | Seconds per tick |
| `start_pts` | integer or null | The first timestamp, in ticks |
| `duration` | time or null | |
| `frames` | integer or null | The frame or packet count the container gives |
| `avg_frame_rate` | rational or null | Frames per second |
| `r_frame_rate` | rational or null | FFmpeg's guess at the base frame rate |
| `bit_rate` | integer or null | Bits per second |
| `disposition` | array of strings | Any of `default`, `dub`, `original`, `comment`, `attached_pic` |
| `handler` | string or null | The track's handler name |
| `language` | string or null | Left out (null) when `und` |
| `video` | object or null | `width` and `height` (integers, 0 when FFmpeg doesn't know them); `sample_aspect` (rational), `pix_fmt`, `bit_depth`, `chroma_subsampling` (`[log2 horizontal, log2 vertical]`, `[1, 1]` for 4:2:0) and `rotation_deg` (the display matrix's rotation), each null when unknown; `color` (`range`, `matrix`, `primaries`, `transfer`, `chroma_location`) and `field_order` (`progressive`, `tt`, `bb`, `tb` or `bt`), FFmpeg's names: `unknown` when the file doesn't say (`unspecified` for `chroma_location`), and null only for a value this FFmpeg has no name for |
| `audio` | object or null | `sample_rate`, `channels`, `layout` and `sample_fmt` (strings or null) |
| `spherical` | object, or absent | Absent when the track has no spherical metadata. `v2` (Spherical Video V2: `stereo` (`mono`, `top_bottom`, `left_right`, `stereo_custom`, `unknown` for a value the specification doesn't define, or null), `stereo_value` (the `st3d` byte it came from, or null), `metadata_source`, `pose_yaw`, `pose_pitch`, `pose_roll` (degrees), `projection` (`{"type": "equirect", "top", "bottom", "left", "right"}` with crop bounds as 0.32 fixed-point fractions, `{"type": "cubemap", "layout", "padding"}`, `{"type": "mesh"}`, `{"type": "other", "fourcc"}`, or null)), `v1` (the V1 XML: `spherical`, `stitched`, `projection_type`, `stereo_mode`, `stitching_software`, `raw_xml`), `spatial_audio` (SA3D: `version`, `ambisonic_type`, `ambisonic_order`, `channel_ordering`, `normalization`, `channel_map`); each null when missing |
| `tags` | object of strings | The stream's metadata |

Text read from the file (tags, handler names, `evidence`, FFmpeg's log)
is the file's, not Spherecut's: treat it as untrusted.

Example: a good clip and a missing file, printed as one line, shown here
indented:

```json
{
  "schema": "spherecut.probe/1",
  "ok": false,
  "result": {
    "files": [
      {
        "path": "grid-h264-25.mp4",
        "ok": true,
        "report": {
          "path": "grid-h264-25.mp4",
          "file_size": 71039,
          "container": {
            "format": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long": "QuickTime / MOV",
            "duration": {"exact": "1", "seconds": 1.0},
            "start_time": {"exact": "0", "seconds": 0.0},
            "bit_rate": 568312,
            "tags": {"compatible_brands": "isomiso2avc1mp41", "encoder": "Lavf60.16.100", "major_brand": "isom", "minor_version": "512"}
          },
          "streams": [
            {
              "index": 0, "kind": "video", "codec": "h264", "codec_tag": "avc1", "profile": "High", "level": 31,
              "time_base": "1/12800", "start_pts": 0,
              "duration": {"exact": "1", "seconds": 1.0},
              "frames": 25, "avg_frame_rate": "25", "r_frame_rate": "25", "bit_rate": 361416,
              "disposition": ["default"], "handler": "VideoHandler", "language": null,
              "video": {
                "width": 1024, "height": 512, "sample_aspect": null, "pix_fmt": "yuv420p", "bit_depth": 8,
                "chroma_subsampling": [1, 1],
                "color": {"range": "tv", "matrix": "bt709", "primaries": "bt709", "transfer": "bt709", "chroma_location": "center"},
                "field_order": "progressive", "rotation_deg": null
              },
              "audio": null,
              "spherical": {
                "v2": {
                  "stereo": "mono", "stereo_value": 0, "metadata_source": "Spherecut",
                  "pose_yaw": 0.0, "pose_pitch": 0.0, "pose_roll": 0.0,
                  "projection": {"type": "equirect", "top": 0, "bottom": 0, "left": 0, "right": 0}
                },
                "v1": null,
                "spatial_audio": null
              },
              "tags": {"handler_name": "VideoHandler", "language": "und", "vendor_id": "[0][0][0][0]"}
            },
            {
              "index": 1, "kind": "audio", "codec": "aac", "codec_tag": "mp4a", "profile": "LC", "level": null,
              "time_base": "1/48000", "start_pts": 0,
              "duration": {"exact": "1", "seconds": 1.0},
              "frames": 48, "avg_frame_rate": null, "r_frame_rate": null, "bit_rate": 184472,
              "disposition": ["default"], "handler": "SoundHandler", "language": null,
              "video": null,
              "audio": {"sample_rate": 48000, "channels": 2, "layout": "stereo", "sample_fmt": "fltp"},
              "tags": {"handler_name": "SoundHandler", "language": "und", "vendor_id": "[0][0][0][0]"}
            }
          ],
          "mp4": {
            "brand": "isom",
            "top_level": [["ftyp", 32], ["free", 8], ["mdat", 68736], ["moov", 2263]], "top_level_omitted": 0,
            "trailing_bytes": 0, "truncated": null, "warnings": []
          },
          "camera": {
            "vendor": null, "model": null, "firmware": null, "layout": "equirect_tagged",
            "evidence": ["spherical video metadata present", "encoder = Lavf60.16.100"]
          },
          "structure": {"check": "full"},
          "damage": [],
          "warnings": [],
          "ffmpeg_log": []
        }
      },
      {
        "path": "missing.mp4",
        "ok": false,
        "error": {
          "code": "not_found",
          "message": "missing.mp4: No such file or directory (os error 2)",
          "path": "missing.mp4"
        }
      }
    ]
  },
  "error": {
    "code": "files_failed",
    "message": "1 of 2 files failed: missing.mp4: No such file or directory (os error 2)"
  },
  "warnings": []
}
```

A file cut short, which FFmpeg still opens, has both its report and its
error; parts of that entry:

```json
"error": {"code": "truncated", "message": "bad-trunc-end.mp4 is damaged: the file ends 4847 bytes into the 4947-byte `moov` box at offset 64815: it was cut short", "path": "bad-trunc-end.mp4"}
"damage": [{"code": "truncated", "message": "the file ends 4847 bytes into the 4947-byte `moov` box at offset 64815: it was cut short"}]
"truncated": {"fourcc": "moov", "offset": 64815, "header_len": 8, "size": 4947, "present": 4847}
```

## `spherecut doctor [--render-node <PATH>] --json`

`result` is doctor's report, whether or not it found errors:

| Field | Type | What |
|---|---|---|
| `distro` | string | `fedora`, `arch` or `other`, which words the fixes |
| `ffmpeg_version` | string | |
| `libraries` | object of strings | Each FFmpeg library's version, such as `"libavcodec": "60.31.102"` |
| `hw_device_types` | array of strings | The hardware device types this FFmpeg was built with |
| `decoders`, `encoders` | arrays | The codecs doctor looks for: `name`, `available` (bool), `hw` (the device types a decoder can use; empty for encoders) |
| `gpu` | object | `render_node` (the node checked for VA-API), `vendor` (`amd`, `intel`, `other` or `unknown`), `vendor_id` (`"0x1002"`, or null) |
| `devices` | array | Each hardware device opened: `kind` (`vaapi`, `vulkan`), `device` (or null for the default), `ok`, `error` (or null), `name` (for `vulkan`, the Vulkan device the context runs on, which may be a CPU device such as llvmpipe; null for `vaapi`, and when it didn't open), `ffmpeg_log` |
| `vulkan_devices` | array | Every Vulkan device FFmpeg's Vulkan instance lists: `name`, `device_type` (`integrated_gpu`, `discrete_gpu`, `virtual_gpu`, `cpu` (a software rasterizer: no GPU acceleration) or `other`), `vendor_id` and `device_id` (hex strings), `api_version`, `driver` and `driver_info` (or null), `used_by_ffmpeg` (whether doctor's Vulkan checks ran on it: FFmpeg's default Vulkan device, which on a machine with two GPUs need not be the GPU behind `render_node`, D38) |
| `decode_checks`, `encode_checks` | arrays | One decode or encode of one format on one path: `format` (such as `HEVC Main 10`), `path` (`software`, `vaapi` or `vulkan`), `codec` (the FFmpeg decoder or encoder), `result` (`works`, `fails` or `not_tested`), `reason` (or null), `fault` (`ffmpeg`, `driver`, `wrong_output`, or null), `ffmpeg_log` |
| `issues` | array | Each problem: `severity` (`error`, `warning` or `info`), `what`, `impact`, `fix` (for people: lines separated by `\n`, each a shell command, the exact `dnf` or `pacman` command where a package fixes it, or a sentence where no command does. A command may be followed by three spaces and a note, as in `sudo dnf swap mesa-vulkan-drivers mesa-vulkan-drivers-freeworld   (RPM Fusion free: ...)` or `sudo usermod -aG render "$USER"   then log out and back in`, so the command is what comes before the first three spaces. Nothing marks a sentence apart from a command: don't run a line unread) |

The run fails with `doctor_errors` exactly when an issue's severity is
`error`; the report comes with the error. Example, on a machine with no
GPU (lavapipe only), with the lists shortened:

```json
{
  "schema": "spherecut.doctor/1",
  "ok": true,
  "result": {
    "distro": "other",
    "ffmpeg_version": "6.1.1-3ubuntu5",
    "libraries": {"libavcodec": "60.31.102", "libavformat": "60.16.100", "libavutil": "58.29.100", "libswresample": "4.12.100", "libswscale": "7.5.100"},
    "hw_device_types": ["vdpau", "cuda", "vaapi", "qsv", "drm", "opencl", "vulkan"],
    "decoders": [{"name": "hevc", "available": true, "hw": ["cuda", "vaapi", "vdpau", "vulkan"]}],
    "encoders": [{"name": "libx264", "available": true, "hw": []}],
    "gpu": {"render_node": "/dev/dri/renderD128", "vendor": "unknown", "vendor_id": null},
    "devices": [
      {"kind": "vaapi", "device": "/dev/dri/renderD128", "ok": false, "error": "Invalid argument", "name": null,
       "ffmpeg_log": ["[AVHWDeviceContext] No VA display found for device /dev/dri/renderD128."]},
      {"kind": "vulkan", "device": null, "ok": true, "error": null, "name": "llvmpipe (LLVM 20.1.2, 256 bits)", "ffmpeg_log": []}
    ],
    "vulkan_devices": [
      {"name": "llvmpipe (LLVM 20.1.2, 256 bits)", "device_type": "cpu", "vendor_id": "0x10005", "device_id": "0x0000",
       "api_version": "1.4.318", "driver": "llvmpipe", "driver_info": "Mesa 25.2.8-0ubuntu0.24.04.4 (LLVM 20.1.2)", "used_by_ffmpeg": true}
    ],
    "decode_checks": [
      {"format": "H.264 High", "path": "software", "codec": "h264", "result": "works", "reason": null, "fault": null, "ffmpeg_log": []},
      {"format": "H.264 High", "path": "vulkan", "codec": "h264", "result": "fails", "reason": "the Vulkan driver cannot decode this format", "fault": "driver",
       "ffmpeg_log": ["[h264] Device does not support the VK_KHR_video_decode_queue extension!"]}
    ],
    "encode_checks": [
      {"format": "H.264", "path": "vaapi", "codec": "h264_vaapi", "result": "not_tested", "reason": "no VA-API device (Invalid argument)", "fault": null, "ffmpeg_log": []}
    ],
    "issues": [
      {"severity": "warning", "what": "no DRM render node at /dev/dri/renderD128",
       "impact": "decode and encode fall back to the CPU, which is far slower at 5.7K and up",
       "fix": "no GPU driver is loaded; in a VM or container, pass the GPU through (for example `--device /dev/dri`)"}
    ]
  },
  "warnings": []
}
```

With an error, `ok` is false, the exit status is 1, and the envelope
adds:

```json
"error": {"code": "doctor_errors", "message": "doctor found 1 error: FFmpeg has no HEVC decoder"}
```

## `spherecut frame <FILE> -o <OUT.png> [options] --json`

One frame of a stitched equirect, rendered through a virtual camera
into a PNG (`spherecut frame --help` lists the options; the README shows
them in use). Without `--json`, a run that wrote the frame prints one
line on stdout (`wrote out.png: 1920×1080 rectilinear 100° at 0.4 s
(frame 12, pts 12012), decoded on VA-API, rendered on ...`), and its
warnings and errors go to stderr, as log lines and an `error:` line.

The run, in order: the command line; the output (refused before any
work if something is there, below); probe, through the same confinement
as `spherecut probe` (D24), and the layout check; the GPU (`--adapter`),
or the CPU reference with `--reference`; the decoder (`--decoder`, on
the rendering GPU's render node, D39); an exact seek; the render; the
PNG. Whatever fails first is the run's `error`, and nothing is written.

`result`, only when the PNG was written:

| Field | Type | What |
|---|---|---|
| `output` | string | The PNG written, as given to `-o` |
| `frame` | object | The frame rendered: `pts` (integer, in ticks of the stream's time base) and `time` (a frame time, in clip time) |
| `requested` | frame time | What `--time` or `--frame` asked for (0 when neither was given). With `--time`, a plain time; with `--frame N` (or `--time Nf`), `frame` is N, and the time is that frame's start on a constant-rate stream. `--time 0.5` shows the frame on screen at 0.5 s: the last one that starts at or before it, so `frame.time` may be earlier than `requested` |
| `exactness` | string | `exact`: the frame is the one asked for. The only value in version 1; a seek that can't be exact fails the run instead |
| `size` | object | `width`, `height`: the PNG's, as `--size` asked (16 to 16384 each, and within the GPU's texture limit) |
| `depth` | integer | Bits per channel of the PNG: 8 or 16 (`--depth`) |
| `view` | object | The virtual camera as given: `yaw` (degrees to the right), `pitch` (up), `roll` (clockwise as seen from behind the camera), `fov` (horizontal, degrees), `projection` (`rectilinear`, `fisheye` or `stereographic`), `morph_to` (one of those, or null) and `morph` (0 is `projection`, 1 is `morph_to`) |
| `filter` | string | `iso` or `aniso`: the filter rendered with, `iso` where `--filter aniso` met a device without anisotropy (a warning says so) |
| `renderer` | string | `gpu`, or `reference` for `--reference`'s CPU render |
| `decode` | object | `path` (`software`, `vaapi` or `vulkan`: where the frame was decoded) and, when a hardware decode went on in software, `fallback`: `from` (`vaapi` or `vulkan`), `code` (`hw_unavailable`, no device; `hw_unsupported`, the device turned the stream down; `hw_failed`, it failed on a frame) and `reason` (for people). `--decoder auto` on a machine without a render node has `fallback` without trying the device |
| `gpu` | object, or null | The adapter rendered on: `name`, `driver`, `driver_info` (strings, as Vulkan gives them), `device_type` (`discrete_gpu`, `integrated_gpu`, `virtual_gpu`, `cpu` for a software rasterizer such as lavapipe, or `other`). Null for `--reference` |
| `color` | object | The colour the frame was decoded as: `codes` (the file's H.273 codes as FFmpeg numbers them: `primaries`, `transfer`, `matrix`, `range`, `chroma_location`, integers), and what the renderer used: `range` (`limited` or `full`), `matrix` (`bt709`, `bt601`, `bt2020_ncl`, `smpte240m`, `fcc` or `identity`), `transfer` (`sdr`, `pq`, `hlg` or `linear`), `primaries` (`bt709`, `bt2020` or `other`), `siting` (`left`, `center`, `top_left`, `top`, `bottom_left` or `bottom`), and `assumed`: for each of those five, whether a default rule chose it because the file left it unspecified (D30) |
| `source` | object | The stream the frame came from: `stream` (FFmpeg's index; `--stream` picks one, the first video stream otherwise), `codec` (FFmpeg's name, such as `hevc`), `size` (`width`, `height`: the decoded picture, after the codec's cropping) and `pix_fmt` (the decoder's pixel format, as FFmpeg names it, such as `yuv420p10le`; a hardware decode names the format it downloads, such as `nv12` or `p010le`) |

Every field is there in every result; `morph_to` and `gpu` are the
fields that may be null, and `fallback` and `frame.time.frame` the ones
that may be absent.

**The output.** Without `--overwrite`, anything at `-o` fails the run
with `output_exists` before any work, a link included (even one to
nothing), and the PNG is written to a temporary file next to it and
then linked into place only if nothing has appeared there since (D16):
a file that appeared while the frame was rendered is left alone, and
the run fails with `output_exists`. With `--overwrite`, links are
followed and a regular file there is replaced in one rename. A
directory, pipe, socket or device at `-o` is `not_a_file` either way. A
failed or interrupted run leaves no output and no temporary file;
only a second Ctrl+C, which kills the process, may leave a temporary
file (named `.spherecut-<16 hex digits>.tmp`) beside the output.

**Ctrl+C.** frame handles SIGINT; probe and doctor don't. The first
Ctrl+C stops the run wherever it is, a long seek included, and the run
ends with `cancelled`, exit 4, and its document. The handler is
installed once, with the default action restored as it runs, so a
second Ctrl+C ends the process at once, with no document (130).

Example, on a machine without a GPU or a render node (lavapipe), as one
line, shown here indented:

```json
{
  "schema": "spherecut.frame/1",
  "ok": true,
  "result": {
    "output": "out.png",
    "frame": {"pts": 14014, "time": {"exact": "7007/15000", "seconds": 0.46713333333333334, "frame": 14}},
    "requested": {"exact": "1/2", "seconds": 0.5},
    "exactness": "exact",
    "size": {"width": 1280, "height": 720},
    "depth": 8,
    "view": {"yaw": 30.0, "pitch": -10.0, "roll": 0.0, "fov": 90.0, "projection": "rectilinear", "morph_to": null, "morph": 0.0},
    "filter": "iso",
    "renderer": "gpu",
    "decode": {
      "path": "software",
      "fallback": {"from": "vaapi", "code": "hw_unavailable", "reason": "no VA-API device: no render node at /dev/dri/renderD128"}
    },
    "gpu": {"name": "llvmpipe (LLVM 20.1.2, 256 bits)", "driver": "llvmpipe", "driver_info": "Mesa 25.2.8-0ubuntu0.24.04.4 (LLVM 20.1.2)", "device_type": "cpu"},
    "color": {
      "codes": {"primaries": 1, "transfer": 1, "matrix": 1, "range": 1, "chroma_location": 2},
      "range": "limited", "matrix": "bt709", "transfer": "sdr", "primaries": "bt709", "siting": "center",
      "assumed": {"range": false, "matrix": false, "transfer": false, "primaries": false, "siting": false}
    },
    "source": {"stream": 0, "codec": "hevc", "size": {"width": 1024, "height": 512}, "pix_fmt": "yuv420p10le"}
  },
  "warnings": []
}
```

A failed run has no `result`:

```json
{"schema":"spherecut.frame/1","ok":false,"error":{"code":"out_of_range","message":"no frame at 1.5 s: the clip runs from 0 to 1.001 s (its last frame starts at 0.968 s)","path":"grid-hevc10-2997.mp4"},"warnings":[]}
```

## A usage error

```sh
$ spherecut probe --json
{"schema":"spherecut.probe/1","ok":false,"error":{"code":"usage","message":"the following required arguments were not provided:\n  <FILES>...\n\nUsage: spherecut probe --json <FILES>...\n\nFor more information, try '--help'."},"warnings":[]}
$ echo $?
2
```

The schema is the subcommand's when the first argument that isn't an
option names one (`probe`, `doctor`, `frame`), and `spherecut/1` otherwise.
frame's own usage errors, which clap can't find (a `--fov` past the
projection's limit, a `--size` past the GPU's, a `--stream` that isn't a
video stream), are `usage` errors with exit 2 too, under
`spherecut.frame/1`.
`--json` counts only before a `--`: in `spherecut probe -- --json`,
`--json` is a file name. Without `--json`, a usage error is clap's text
on stderr, with exit 2.
