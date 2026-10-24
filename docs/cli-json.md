# The CLI's JSON contract

What `spherecut probe --json` and `spherecut doctor --json` print, for
scripts and AI agents. `spherecut frame` joins the same contract in M1,
and A1's MCP tools return the same time shapes and codes. DECISIONS D27
records why it is this way.

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
  and `frame` will catch only the first Ctrl+C. Anything else is a crash,
  such as SIGSEGV or SIGABRT inside FFmpeg (139 or 134): please report
  it.

## Versions

`schema` names the document and its version: `spherecut.probe/1`,
`spherecut.doctor/1`, and `spherecut/1` for a usage error that names no
subcommand. Check the whole string.

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
| `result` | object, or absent | The command's result (below). Absent for a `usage` or `internal` error. probe and doctor have one in every other run, failed or not: a damaged file still has its report, and doctor's errors come with doctor's report. `frame` will say when it has one |
| `error` | error, or absent | Why the run failed. Absent when `ok` is true |
| `warnings` | array of warnings | About the run as a whole. probe and doctor report what they find in `result`, so for them this is always empty in version 1 |

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
  rendered, and A1's tools give them wherever a time is a frame.

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

| Status | Meaning | probe | doctor |
|---:|---|---|---|
| 0 | The run did what was asked | Every file is ok (warnings don't count) | No problem of severity error |
| 1 | An input or a check failed, or Spherecut itself did (`internal`) | At least one file failed | At least one problem is an error: no H.264, HEVC, ProRes or AAC decoder, or nothing that decodes AV1 (D18) |
| 2 | Usage error: the command line doesn't parse | yes | yes |
| 3 | Reserved: a codec or device the command itself needs is missing | never | never |
| 4 | Reserved: interrupted (SIGINT), leaving no output file; the document's code is `cancelled` | never | never |

3 is for `spherecut frame` (no GPU, an unknown adapter, `--decoder
vaapi` without a working device, no decoder for the codec) and for the
GUI (no display). 4 is for `frame`. A missing decoder is a warning in
probe and a finding in doctor, so neither exits 3: doctor checks
capability and reports it.

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

A file's `error.code` in probe (`result.files[].error.code`):

| Code | Has a report | Meaning |
|---|---|---|
| `not_found` | no | No such file, including a path that looks like a URL (`http://host/x.mp4` is a path) |
| `io_error` | no | Another error reading it, such as permission denied, a read error from a failing disk or card, or a path through a regular file. Never `damaged`: the file may be fine |
| `not_a_file` | no | A directory, pipe, device or socket |
| `not_media` | no | Empty, or FFmpeg doesn't know it as media |
| `unsupported_container` | no | FFmpeg reads it as a container Spherecut doesn't open (an HLS playlist, an ffconcat script, WAV, ...); the message names it (D24) |
| `nested_open_refused` | no | Opening it, FFmpeg tried to open another file or URL, which is refused (D24) |
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

Errors of the decoders, which probe and doctor don't give, and which
`frame` will (each exit 1 unless `frame` says otherwise; `frame` also
gives probe's file codes above for the file it opens):

| Code | Meaning |
|---|---|
| `decode_failed` | A decoder failed |
| `too_large` | As the warning above, as an error: the decoder refuses the stream |
| `hw_unsupported` | The hardware decoder refuses the stream; software may still decode it (exit 3 for `frame --decoder vaapi`) |
| `hw_unavailable` | The hardware device can't be opened (exit 3 for `frame --decoder vaapi`) |

Planned with `frame` (M1 slice 4) and the GUI, and not given yet:
`output_exists`, `not_equirect`, `out_of_range` (exit 1); `no_gpu`,
`adapter_not_found`, `no_decoder` as an error, `no_display` (exit 3);
`cancelled` (exit 4). They join the tables above when they land.

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
| `ffmpeg_log` | array of strings | FFmpeg's warnings and errors while reading the file, each one whole line starting with the context that logged it (`[aac] ...`); a repeated line once with a count, at most 32. Healthy files can have some |

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
                "color": {"range": "tv", "matrix": "bt709", "primaries": "bt709", "transfer": "bt709", "chroma_location": "left"},
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

## A usage error

```sh
$ spherecut probe --json
{"schema":"spherecut.probe/1","ok":false,"error":{"code":"usage","message":"the following required arguments were not provided:\n  <FILES>...\n\nUsage: spherecut probe --json <FILES>...\n\nFor more information, try '--help'."},"warnings":[]}
$ echo $?
2
```

The schema is the subcommand's when the first argument that isn't an
option names one (`probe`, `doctor`), and `spherecut/1` otherwise.
`--json` counts only before a `--`: in `spherecut probe -- --json`,
`--json` is a file name. Without `--json`, a usage error is clap's text
on stderr, with exit 2.
