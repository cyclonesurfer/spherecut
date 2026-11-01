# Spherecut for AI agents

An AI agent on this machine (Claude Code, Codex, Gemini CLI, Copilot
CLI, Cursor, Zed, goose, opencode, or anything else that starts a local
MCP server or runs a command) can go through your footage with
Spherecut: probe a folder, look at any moment from any direction, find
where something happens, explain stutter or audio problems, and check a
file's 360 metadata. Given a write root it can also make projects of
your clips, edit and undo them, and render them ([Projects](#projects));
without one it only reads.

There are two ways in, with the same tools, arguments and results:

- **`spherecut mcp`**, a Model Context Protocol server on stdin and
  stdout, which the agent starts itself (setup below).
- **`spherecut call <tool> '<json>'`**, for agents and scripts that only
  run commands. It prints one JSON document and writes images to files.

## What leaves this machine

Spherecut makes no network requests; it opens no port and loads nothing.
**But whatever a tool returns goes to the agent, and from there to the
agent's model provider**: file names, metadata, and frames of your
footage rendered as images. Point an agent only at footage you are happy
to send to its provider, under the provider's terms.

## What the roots do, and what they don't

`spherecut mcp` reads files only under its read roots
(`--read-root DIR`, repeatable; the working directory by default) and,
with `--allow-write --write-root DIR`, writes new files only under the
write root, which it reads from too. The kernel enforces this: every
file is opened with `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`
beneath its root, so `..`, an absolute path outside, a symlink out of a
root, a dangling symlink and `/proc/self/fd/N` are all refused
(`outside_roots` or `not_found`), and a file swapped after it was opened
is not the one read (DECISIONS D45). Nothing is overwritten but a
project: an output is a new file, and anything already at its name, a
symlink included, is `output_exists`; `edit` and `undo` replace only a
file that loads as a project, and the hidden files beside it
([Projects](#projects)). No tool deletes or moves a file of yours or
starts a process, and tools take only data, never code, filters or
commands.

**The roots limit what Spherecut opens for an agent, not what the agent
can do.** An agent that can run shell commands can read your files
without Spherecut. The roots keep Spherecut from becoming a way around
the agent's own sandbox, and nothing more.

## Running it

```
spherecut mcp [--read-root DIR]... [--allow-write --write-root DIR] [--image-budget-kb KB]
spherecut call TOOL ['{"json": "args"}'] [same flags] [--image-dir DIR]
spherecut agents install-skill [--dry-run]
```

- `--read-root DIR`: a folder the agent may read, and everything under
  it; repeat it for more. A relative path from the agent is under the
  first. Without it, the working directory the client starts the server
  in, which is often not your footage: name your footage folder.
- `--allow-write --write-root DIR` (both or neither): lets `look` write
  its image to a new file under `DIR` when the call gives `output`, and
  lists `edit`, `undo`, `render` and `job`: projects are made, edited
  and rendered only under `DIR`.
- `--image-budget-kb KB` (default 60, at most 5000): the most base64 an
  image in a result may take. See [Images and output limits](#images-and-output-limits).
- `spherecut call` prints `{"schema": "spherecut.call/1", "ok", "result",
  "error", "warnings", "images"}` and exits 0, 1 for a tool's error, 2
  for usage (bad JSON, an unknown tool, `bad_argument`), 3 for `no_gpu`,
  4 on Ctrl-C (D27). A decoder FFmpeg lacks is a tool's error,
  `decode_failed`, so 1. Images go to `--image-dir`
  (the temporary folder by default) as new files, listed in `images`.
- `spherecut agents install-skill` writes Spherecut's skill (below) to
  `$XDG_DATA_HOME/spherecut/skills/spherecut/SKILL.md` and links that
  folder as `~/.claude/skills/spherecut` and `~/.agents/skills/spherecut`.
  Anything else already at either name is left alone and reported, and
  the run exits 1. `--dry-run` says what it would do.

Logs go to stderr (`SPHERECUT_LOG=debug` for more). stdout carries
only the protocol.

## The skill

`spherecut agents install-skill` installs an [Agent Skills](https://agentskills.io)
`SKILL.md` with four recipes: find where a moving subject is over time,
validate a 360 export, find why a file stutters, and reframe a clip into
a flat video (which needs a write root). Claude Code reads
`~/.claude/skills`; Codex, Gemini CLI and Copilot CLI read
`~/.agents/skills`. The skill's text is
`crates/agent/skill/spherecut/SKILL.md`; a test replays each recipe on
synthetic media (`crates/agent/tests/recipes.rs`), the fourth through
a project, a draft render, `compare` and the whole render.

## Setting up each client

Each entry starts `spherecut mcp` with your footage folder as the read
root; replace `/home/you/Videos` with yours, and add `--read-root` again
for more. If `spherecut` isn't on the client's `PATH`, give its full
path. No client needs a timeout raised past what is said here: a call
stops itself after 50 s.

**Environment.** Spherecut decodes on the GPU through VA-API and
Vulkan, and reads where to look from the environment where you set it:
`LIBVA_DRIVER_NAME`, `LIBVA_DRIVERS_PATH`, `MESA_VK_DEVICE_SELECT`,
`VK_ICD_FILENAMES`, `XDG_RUNTIME_DIR`, and Spherecut's own
`SPHERECUT_ADAPTER` and `SPHERECUT_LOG`. A client that passes its whole
environment needs nothing. **Codex passes a server only `HOME`,
`LOGNAME`, `PATH`, `SHELL`, `USER`, `LANG`, `LC_ALL`, `TERM`, `TMPDIR`
and `TZ`**, so forward any of these you set (below). Without them
Spherecut still works, and `doctor` says which decode path it found.

### Claude Code

```
claude mcp add --scope user spherecut -- spherecut mcp --read-root /home/you/Videos
```

`--env NAME=value` sets a variable for the server. Claude Code asks for
MCP 2026-07-28 first and falls back to 2025-11-25, which Spherecut
serves. It limits a tool's result to `MAX_MCP_OUTPUT_TOKENS` (25,000 by
default) and counts images against it; Spherecut's default image budget
keeps a look under that. To raise both for larger images, start Claude
Code with `MAX_MCP_OUTPUT_TOKENS=50000 claude` and give the server
`--image-budget-kb 150`.

### Codex

```
codex mcp add spherecut -- spherecut mcp --read-root /home/you/Videos
```

or in `~/.codex/config.toml`:

```toml
[mcp_servers.spherecut]
command = "spherecut"
args = ["mcp", "--read-root", "/home/you/Videos"]
env_vars = ["LIBVA_DRIVER_NAME", "LIBVA_DRIVERS_PATH", "MESA_VK_DEVICE_SELECT", "VK_ICD_FILENAMES", "XDG_RUNTIME_DIR"]
```

`env_vars` forwards those variables from Codex's own environment where
they are set. `tool_timeout_sec` is 60 by default, above Spherecut's
50 s; leave it there or higher. Codex shows images only to models that
take them, and says so in their place otherwise.

### Gemini CLI

In `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "spherecut": {
      "command": "spherecut",
      "args": ["mcp", "--read-root", "/home/you/Videos"]
    }
  }
}
```

`env` sets variables if you need them. Its `timeout` is 600,000 ms by
default. Gemini CLI's MCP library asked for 2025-06-18 when it was
checked (2026-10-24).

### Copilot CLI

```
copilot mcp add spherecut -- spherecut mcp --read-root /home/you/Videos
```

`--env NAME=value` sets a variable; if you give `--timeout`, keep it at
60000 ms or more. The server is written to `~/.copilot/mcp-config.json`.

### Cursor

In `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "spherecut": {
      "type": "stdio",
      "command": "spherecut",
      "args": ["mcp", "--read-root", "/home/you/Videos"]
    }
  }
}
```

### Zed

In Zed's `settings.json`:

```json
{
  "context_servers": {
    "spherecut": {
      "command": "spherecut",
      "args": ["mcp", "--read-root", "/home/you/Videos"],
      "env": {}
    }
  }
}
```

Zed's docs list MCP tools but not images in their results. Every `look`
result also says in text what each tile holds, so an agent in Zed can
still use it.

### goose

`goose configure`, then Add Extension and Command-line Extension, with
the command `spherecut mcp --read-root /home/you/Videos`; or in
`~/.config/goose/config.yaml`:

```yaml
extensions:
  spherecut:
    name: spherecut
    type: stdio
    cmd: spherecut
    args: [mcp, --read-root, /home/you/Videos]
    enabled: true
    timeout: 300
```

### opencode

In `opencode.json`:

```json
{
  "mcp": {
    "spherecut": {
      "type": "local",
      "command": ["spherecut", "mcp", "--read-root", "/home/you/Videos"],
      "timeout": 60000
    }
  }
}
```

opencode's `timeout` (30 s by default) covers tool calls, so raise it
to 60,000 ms.

### A client installed as a Flatpak

A Flatpak client can't run the host's `spherecut` directly. Start it
through the host with `flatpak-spawn --host`: the command is
`flatpak-spawn` and the arguments begin `--host spherecut mcp
--read-root /home/you/Videos`. The client needs permission to talk to
the host (`--talk-name=org.freedesktop.Flatpak`, which Flatseal can
grant), and the paths are the host's.

### Which revision each client used

Spherecut serves MCP 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05
through `initialize`. A request in 2026-07-28's form (`server/discover`,
or the revision in `_meta`) gets -32022 with that list, and Claude Code
falls back. PROGRESS.md records each client's version and the revision
it negotiated when it was tried with Spherecut.

## The tools

Eleven tools, in this order; `edit`, `undo`, `render` and `job` are listed
only with a write root (`--allow-write`), since without one each call
would be refused. Each takes one JSON object; the schemas are
`docs/agents/schemas/*.json`. Every result has `warnings: [{code,
message}]`. Each is marked `readOnlyHint` true and `openWorldHint`
false, except under `--allow-write`: `look` and `project` are marked as
writing new files only (`readOnlyHint` false, `destructiveHint` false,
`idempotentHint` true), since `look` writes one when a call gives
`output` and `project` when it gives `create`, as are `render` and
`job`, and `edit` and `undo` as replacing a project (`destructiveHint`
true, `idempotentHint` false).

- **`probe`**: what media files are, without decoding. `files` (up to
  64) or a `folder` (one level, its clips a page at a time: `page_size`
  20, `cursor` for the next). A summary a file or clip: `duration`,
  `size`, `fps`, `codec`, `bit_depth`, `projection` and `metadata` (what
  says so), `camera` (its name, or null), `audio`, `problems`, and for
  a folder `files` (a camera's lens files, proxy and chapters together)
  and `recorded`. A camera's file adds what it names (`camera_detail`:
  `vendor`, `model`, `firmware`, `serial`) and, for an Insta360 file,
  what its trailer holds, as `docs/cli-json.md`'s probe report has
  them: `lenses` (a master's two lens streams), `calibration` (its
  generations, the one in use, canvas, window and hash), `companion`
  (the master's LRV or the LRV's master beneath the roots, `verified`
  when their exposure records agree byte for byte, else `why`),
  `exposure_entries` and `records`.
  `detail: "full"` adds the whole report and FFmpeg's log, for 8 files
  at most. A file that fails is an entry with `ok: false` and its error.
- **`look`**: frames as one image of labelled tiles, plus JSON with each
  tile's `time`, `rect` and `decode` path (`vaapi`, `vulkan` or
  `software`, and why a fallback happened). `mode`: `view` (a camera:
  `view: {yaw, pitch, roll, fov | vfov, projection}`, or `views`, up to
  24 of one time), `sphere` (the equirect under a labelled 30° grid),
  `cube` (six faces), `sheet` (a small tile a time over the clip, or
  `count` times between two), `raw` (the picture as coded, each video
  stream side by side, for footage not yet stitched). `times` up to 24,
  `size` 64 to 4096 (768 by default, 1568 at most inline), `format`
  `jpeg` or `png`, `seek` `exact` or `keyframe`, `output`,
  `assume_equirect`. With `project` in place of `file`, the tiles are
  the project's render: its frames at the times, which are the
  render's (0 is its first frame, `"12f"` its frame 12), rendered as
  the export renders them (`frame --project`), in `mode` `view` or
  `sheet` (12 over the render by default). Each tile gives `time`, the
  render's, and `source`, the media's. `view`, `views`, `seek` and
  `assume_equirect` are refused there: a view of the source is a look
  at the project's media file.
- **`analyze`**: a measure over time in `buckets` (50 by default, up to
  200), between `from` and `to`. `kind`: `timing` (packets only: frame
  durations, `vfr`, duplicate timestamps, gaps and frames missing,
  repeated pictures, keyframes, Mb/s), `motion` (decodes: change from
  frame to frame, 0 to 255, and likely cuts), `audio` (peak and RMS
  dBFS, clipped samples, silences). The summary's moments (gaps,
  duplicates, repeated pictures, cuts, the most motion, silences) are
  times as `look` takes them back, `{exact, seconds}`, the first 20 of
  each list with its `count`; a bucket's `from` is plain seconds. Out of
  time, it returns the buckets it finished with `complete: false` and
  `next_from`.
- **`check`**: a file's spherical and ambiX metadata against its
  streams, and the file against a `target` (`youtube_360`,
  `flat_16_9`, `vertical`, `archive`). `{target, pass, rules: [{id, ok,
  message, fix}], advice: [{id, message}]}`, under 2 KB. D46 lists every
  rule. With `project` in place of `file`, it checks the file a render
  of the project would make (`checked` says so): the output's kind and
  size, codec, bit depth, colour, audio and its metadata, by the same
  rules and target, before any render. The `check` a finished `job`
  gives is the proof.
- **`doctor`**: this machine's FFmpeg, decoders, VA-API and Vulkan
  devices, and each problem with its fix (the exact Fedora commands come
  from `spherecut doctor` in a terminal). With `file`, the decode path
  `look` and `spherecut frame` take for it, and with `speed: true` its
  decode speed against real time (`realtime`). `render_node` picks a
  DRM node.
- **`project`**: a project (`.spherecut`, D47): with `file` alone, its
  summary (the `file` path the other calls name it by, `revision`, the
  sequence's `rate`, `output` and `export`, each media file with its
  `status`, the clip's trim and its view tracks' key counts, the first
  markers, the targets, and the history's `undo` and `redo`, each with
  `agent_may` and why not). `part` gives one part: `sequence`, `view`
  (a clip's keys, at most 200 a track from `from`; `more` and
  `next_from` say where to go on), `markers`, `targets`, `history` (the
  last 50 steps, who made each), or `edits` (every op with what it does
  and an example that applies to the ids it names). `create: {media}`
  makes a new project of one equirect video, refused `output_exists` if
  anything is at `file`. A relative `file` is read under the first read
  root but made under the write root: the summary's `file` is the path
  to go on with.
- **`edit`**: `edits` (up to 1000) applied in order to the project at
  `file`, all or none, as one undo step labelled `label` (by default the
  first op's name, "Set trim", or "N edits"); the user's Edit menu shows
  it as the agent's. Ops and fields are the project file's, ids are
  `project`'s, and times are as above, frames at the clip's media's rate
  (a target's own media for its path, the sequence's for `add_clip`'s
  `at`). `add_media` takes only `path`, probed beneath the roots. The
  result gives the new `revision`, the ids each edit made, and whether
  the agent may undo it; `if_revision` refuses the edit
  (`stale_revision`) unless the project is still at that revision, and
  `dry_run` checks it all and saves nothing. A refused edit is
  `bad_edit` at `edits[i]`, its op and rule in the message.
- **`undo`**: takes back the last step, or with `redo` puts back the
  last one undone, if the agent made it: a step of the user's is
  `not_top`, and a project written by something else since its history
  was is `stale_revision`. `if_revision` as for `edit`.
- **`render`**: exports the project's sequence to a new `.mp4` beneath
  the write root, as `spherecut render` does, and answers within a
  second, before a frame is rendered, with `{job, output, frames,
  duration, size, draft, encoder, estimate_bytes, ahead}`. First it
  checks the project's media (`media_missing`, `media_changed`), the
  output (`output_exists`, also for a file a render not yet finished
  writes), `range` (`{from, to}`, within the trim, in the clip's file
  time: `out_of_range`), the preset, frames and encoder (D51's
  `cannot_export`, `no_encoder`), and room on the disk (`no_space`, with
  both numbers). `draft` renders at half the size, fast. One renders at
  a time and 4 wait (`busy` past them). The file appears only when it
  is whole and its checks pass, and nothing at the name is replaced or
  followed, a link included. `spherecut call` refuses `render` and
  `job`: a render ends with its server, so it would end with the call;
  `spherecut render --json` is the same export.
- **`job`**: a render's `state` (`queued`, `running`, `finishing`,
  `done`, `failed`, `cancelled`), `frames: {done, total}`, `fps`,
  `seconds_left`, the encoder and any `fallback`; once `done`, its
  `report` (`file`, `bytes`, `codec`, `size`, `duration`, `audio`,
  `warnings`) and a `check` of the file against the target its output
  fits (`youtube_360` for the sphere, `flat_16_9`, `vertical`, and the
  rules alone for 1:1 and 4:5); once `failed`, its `error`, with the
  export's code (`cannot_export`, `output_exists`, `decode_failed`,
  `render_failed`, `encode_failed`, `mux_failed`, `metadata_failed`,
  `check_failed`, `io_error`, `no_space`, the hardware codes, or
  `internal`). `wait`
  (seconds, up to 45) returns when it ends or the time passes, sending
  progress if the call asked for it; one call waits at a time, and
  another answers at once with a `note`. opencode's calls end after
  30 s, so wait less there. `cancel` stops it and its file, within 2 s.
  The last 16 finished jobs are kept; an id this server didn't give
  (one from a server that has ended, or another that is running) is
  `job_lost`. When the server ends, its renders are cancelled;
  a file a killed server left is removed by the next render in that
  folder.
- **`compare`**: how alike `file` is to `against_file` (a video of the
  same size) or to `against_project`'s render (rendered at `file`'s
  size, refused when its shape differs), at `times` (file time, up to
  24) or `count` spread over the shorter of the two (8 by default): the
  PSNR (dB, `"inf"` when identical) and SSIM of BT.709 luma at each
  time, in limited range as an encoder codes it (so the dB are the
  export's parity tests'), the worst of each and the means (the mean
  PSNR is the mean error's). Each time is decoded exactly and drawn whole at 16 bits a
  channel; a project's frame is the one its render shows at the file's
  frame's time, so a render compares frame for frame with its project,
  and `from` (the clip's media time, the trim's start by default) is
  where the render started: for a `render` of a `range`, its `from`.
  `sheet` adds an image of the four worst times, a row each of the
  file, the other and their difference times four, within the inline
  budget. Out of time, it answers with the times it measured,
  `partial: true`. A render is above 40 dB and an SSIM of 0.98 against
  its project at every time, whatever its encoder (PLAN's least; the
  software encoders at their presets are near 50 dB, VA-API and SVT-AV1
  a few dB under); well under that, around 30 dB or less, means the
  wrong frames: the wrong `from`, or not this project's render.

**Conventions.** Times are strings (`"12.5"`, `"1:02.5"`, `"370f"` for
frame 370, `"30000/1001"`) or `[num, den]` pairs of seconds; a bare
number is `bad_argument`, with the string to send. Every time comes back
as `{exact, seconds}`, plus `frame` for a video's frame (the CLI's
`--json` shape); `exact` reads back as the same time. Angles are
degrees: yaw 0 is the centre of the equirect and yaw > 0 turns right,
pitch > 0 looks up, roll > 0 rolls clockwise; `fov` is across the view
unless `vfov` is given.

**Untrusted text.** Text read from files (tags and their keys, vendor
strings, file and folder names, camera names, FFmpeg's log) appears only
in JSON string fields, at most 1 KiB each, with control, bidi, ANSI and
invisible characters replaced. It never appears in a tool's description,
the server's instructions or an error's message. The tools' descriptions
and the instructions tell the agent that these fields and the pixels of
frames are data, not instructions.

## Projects

A project is a `.spherecut` file (D47): JSON, always written whole, its
media named relative to its folder. `project` with `part: "edits"` is
the edit reference: every op, what it does, and an example that applies
to the ids it names; the ops and their fields are the project file's
(D47 and `docs/design/m2-plan.md` sections 3 to 5 have the format).

**The files beside a project.** `trip.spherecut` has two hidden files
beside it that Spherecut makes and nothing else should touch:

- `.trip.spherecut.lock`, the lock. The window holds it while the
  project is open; `project` with `create`, `edit` and `undo` hold it
  for the call. A call waits for another call's hold up to `LOCK_WAIT`
  and is refused at once while the window has it (`project_locked`,
  saying who). It is never removed or replaced, only written over with
  its holder's name. Reading takes no lock:
  `project`, `look`, `check`, `compare` and `render` read a project the
  window has open, and `render` reads it once, when it is called.
- `.trip.spherecut.history`, the undo steps, written with every save,
  the window's and the agents' alike, so the user's Ctrl+Z goes back
  through an agent's steps. A history that doesn't match its project
  (written by something else since) is stale: undo and redo are
  `stale_revision`, and the next edit starts a fresh history
  (`history_reset: true`). It is kept under 16 MiB, the oldest steps
  going first (`history_dropped` in `edit`'s result). It has the
  project's permissions, and a project's save fails, leaving it as it
  was, if its history can't be written.

A project changed on disk since a call read it (by a text editor, git,
a sync tool) is never saved over: `changed_on_disk`, and nothing is
written. Beneath the write root Spherecut replaces only a file that
loads as a project, and its history; everything else it writes is new.

**Undo.** `undo` takes back the last step only if an agent made it (any
agent's: `spherecut call` and every client count as agents), and `redo`
puts back the last one undone the same way. A step of the user's
is `not_top`: theirs to undo, in the window. The summary's `undo` and
`redo` say whether an agent may (`agent_may`) and why not. The window's
Edit menu names the client an agent's step came from ("Undo Set trim
(claude-code)").

**The window's actions that aren't edits**, and what an agent does
instead:

| In the window | An agent |
|---|---|
| Play, pause, scrub, step a frame | `look` at the time (with `project`, the render's) |
| Jump a second (Shift+arrows), to the start or end (Home, End), to the previous or next key (Alt+[ and Alt+]) | `look` at that time; `project` with `part: "view"` lists the keys' times |
| Drag the viewer to look around | `look` with a `view`; to keep it, `edit` with `add_key`, `set_key` or `set_view_path` |
| Zoom (scroll, + and −), the lens presets (1, 2, 3, the transport's lens buttons, Morph to), reset the view (R) | `look` with a view's `fov` and `projection` (the default view for a reset); to keep it, `edit` with a key on the `fov` or `projection` track |
| The curve editor's handles | `edit` with `set_key` and its `ease` |
| Open, Save, Save As, Close | `project` (`create` for a new one); every `edit` and `undo` saves |
| Replace an existing file (Save As's and Export's "Replace it?") | None: an agent's new file never replaces one (`output_exists`); it gives another name |
| Undo, Redo | `undo` (with `redo`), on an agent's steps |
| Export | `render`, then `job` |
| The Info and Files panes | `probe` |
| The Doctor pane | `doctor` |
| The overlay (F3), Settings (anisotropic filtering among them), the sphere overview, the equirect preview, loop (Ctrl+L), the panes' layout, Help, Quit | None: they change no project and no file |

Each milestone's review checks this list against the window until M5's
shortcut map lets a test do it (PLAN).

## Errors

A failed call is a tool result marked `isError`, whose text is
`{"error": {code, argument, message, valid, next}}`, under 500
characters: a stable `code`, the argument at fault as a JSON path
(`file`, `times[3]`, `view.fov`) or null, what would be valid, and what
to try next. A message may name a file or folder the call gave (a
folder without room, a media file that moved) and, for a render that
failed writing, the writer's words; probe's `detail: "full"` gives
FFmpeg's own. The codes:

| Code | Meaning |
|---|---|
| `bad_argument` | Wrong type, unknown or missing, or a time as a bare number |
| `out_of_range` | Outside its range: a time past the end, too many files or tiles |
| `outside_roots` | Outside every read root (or the write root, for `output`), or leading out of one |
| `not_found` | Nothing there, a dangling link, or not readable |
| `not_a_file` | Not a regular file (a folder, a pipe, a device) |
| `not_a_folder` | Not a folder |
| `unsupported_container` | Empty, or not MP4, MOV, Matroska or WebM |
| `no_video` | No video stream where one is needed |
| `no_audio` | No audio stream, for `analyze audio` |
| `not_equirect` | A view of a file that isn't a tagged equirect; `next` says `raw` or `assume_equirect` |
| `damaged` | Cut short or corrupt where the call needed it |
| `decode_failed` | A frame or stream that won't decode, or no decoder for it here |
| `render_failed` | The GPU failed to render, or ran out of memory |
| `no_gpu` | No GPU, not even Mesa's software Vulkan (lavapipe) |
| `image_too_large` | An image over the inline cap or budget, with no `output` to write it to |
| `write_refused` | Writing is off (no `--allow-write`), or the file system refused the file |
| `output_exists` | Something is already at `output`; nothing is replaced |
| `cursor_stale` | The folder changed between pages; start again without `cursor` |
| `timed_out` | The call ran past 50 s; `next` says how to ask for less |
| `busy` | Too many calls waiting already, or 4 renders; call again |
| `cancelled` | The call was cancelled; the client gets no response for it (a cancelled render's `state`, too) |
| `internal` | Spherecut's fault, not the agent's |
| `bad_edit` | An edit the project's rules refuse, at `edits[i]`: its op and the rule (`limit`, `in_use`, `not_found` and so on) are in the message; nothing was saved |
| `stale_revision` | The project isn't at `if_revision`, or was written by something else since its history was; read it again |
| `not_top` | The step to undo or redo is the user's; ask the user, or make an edit that changes it |
| `nothing_to_undo` | No step to undo |
| `nothing_to_redo` | No step to redo |
| `project_locked` | Spherecut's window has the project open, or another agent's call is writing it |
| `changed_on_disk` | Something other than Spherecut wrote the project during the call; nothing was saved over it |
| `not_project` | Not a Spherecut project; it was left as it was |
| `bad_project` | A project that doesn't read: malformed, or breaking a rule |
| `newer_schema` | A project saved by a newer Spherecut |
| `io_error` | Reading or writing a project or a render's file failed partway (the disk, a permission); nothing was saved |
| `media_missing` | A project's media file isn't there, or isn't beneath the roots |
| `media_changed` | A project's media file isn't the one it was made with: its size or first MiB differ |
| `no_space` | The output's disk hasn't room for the render, with both numbers |
| `job_lost` | A job this server didn't give (one that has ended, or another running); render again |
| `cannot_export` | A render the project can't make: its output isn't a preset, or its trim has no frames |
| `no_encoder` | No encoder here for the project's codec |
| `hw_unavailable`, `hw_unsupported`, `hw_failed` | A render's hardware encoder isn't there, turned the job down, or failed, with nothing to fall back to |
| `encode_failed`, `mux_failed`, `metadata_failed`, `check_failed` | A render's stage failed: the encoder, the muxer, the metadata boxes, the finished file's checks |

How the codes of media and render (the CLI's `--json`, D27, and
`docs/cli-json.md`) become these:

| media or render code | Becomes |
|---|---|
| `not_found` | `not_found` |
| `not_a_file` | `not_a_file` |
| `io_error` | `damaged` |
| `open_failed`, `damaged`, `truncated` | `damaged`; for `check`, a failed `readable` rule |
| `not_media`, `unsupported_container`, `nested_open_refused` | `unsupported_container` |
| `no_stream`, `no_video_or_audio` | `no_video` (`no_audio` for `analyze audio`) |
| `decode_failed` | `decode_failed`, at a time; `no_video` while opening |
| `no_decoder`, `unsupported_pixel_format`, `not_implemented` | `decode_failed` (`no_decoder` is also a probe problem) |
| `too_large` | `decode_failed` (a picture over `MAX_PIXELS`) |
| `hw_unsupported`, `hw_failed`, `hw_unavailable` | usually no error: the call falls back to software, and `decode.fallback` says why; `decode_failed` if it can't |
| `cannot_decode`, `unexpected_first_box`, `track_without_stream`, `bad_trailer` | probe problems, not errors |
| `no_gpu`, `adapter_not_found` | `no_gpu` |
| `out_of_gpu_memory`, `gpu_failed`, `import_failed`, `invalid_input` | `render_failed` |
| `internal` | `internal` |

JSON-RPC errors are only for requests that are malformed or unknown:
-32700 for JSON that doesn't parse (with a null id; the next line is
read), -32600 for a request before `initialize`, a batch, or a line over
1 MiB, -32601 for an unknown method, -32602 for an unknown tool, and
-32022 for 2026-07-28's form.

## Images and output limits

An image inline is a JPEG, 768 px on its long edge by default and at
most 1568 (`size`), of at most 60 KB of base64 (`--image-budget-kb`):
its quality steps down to fit, and past the coarsest it is
`image_too_large`. PNG is there on request (`format: "png"`), within the
same budget. A larger image, up to 4096 px, needs `output` and
`--allow-write`. Each result gives the image's `width`, `height`,
`bytes`, `format` and JPEG `quality`.

60 KB of base64 is 15,000 tokens even for a client that counts it as
text, under Claude Code's 25,000. A model reads an image as about
(width / 28) × (height / 28) tokens, so a 768 × 432 sheet is about 450.
Text results stay under 40,000 characters, below the 50,000 where
Claude Code saves a result to a file instead.

## Limits

Every limit is a constant in `crates/agent/src/limits.rs`, each with a
test; a test checks that each is listed here.

| Limit | Value | What |
|---|---|---|
| `MAX_REQUEST_BYTES` | 1 MiB | the longest message read; a longer line gets -32600 |
| `CALLS_AT_ONCE` | 2 | tool calls that run at once; `ping` and `tools/list` are answered meanwhile |
| `MAX_QUEUED` | 8 | calls that may wait; one more is `busy` |
| `CALL_TIME` | 50 s | a call's time from its request, under Codex's 60 s; then `timed_out` |
| `STUCK_GRACE` | 2 s | past the deadline, a call that hasn't stopped is answered `timed_out` and its worker abandoned |
| `MAX_ABANDONED` | 4 | abandoned workers before the server exits 1 for the client to restart it |
| `EXIT_GRACE` | 1.5 s | how long the server waits for running calls once stdin closes |
| `PROGRESS_EVERY` | 250 ms | the fewest between two progress notifications of a call |
| `FILES_PER_CALL` | 64 | files in one `probe` |
| `FULL_PER_CALL` | 8 | files or clips `probe` gives in full |
| `PAGE_SIZE` | 20 | a folder's clips in a page, by default |
| `MAX_PAGE_SIZE` | 50 | the most a page may hold |
| `FOLDERS_CHARS` | 4,000 | characters of subfolder names a page lists |
| `FOLDER_ENTRIES` | 10,000 | entries a folder's listing keeps |
| `RESULT_CHARS` | 40,000 | the most characters a result's text takes |
| `FIELD_BYTES` | 1 KiB | a string read from a file; longer ones end in `…` |
| `ERROR_CHARS` | 500 | an error's JSON |
| `CHECK_BYTES` | 2 KiB | a `check` result; passed rules' messages, then advice, give way |
| `SPEED_TIME` | 5 s | doctor's `speed` decodes for at most this |
| `MAX_TILES` | 24 | tiles in one `look`: times × views |
| `IMAGE_DEFAULT_EDGE` | 768 px | `look`'s long edge by default |
| `IMAGE_MAX_EDGE` | 1568 px | the longest edge inline |
| `IMAGE_LIMIT_EDGE` | 4096 px | the longest edge at all, to a file |
| `MIN_TILE_WIDTH` | 112 px | the narrowest tile, for its label |
| `INLINE_IMAGE_BYTES` | 60,000 | base64 an inline image may take, by default |
| `MAX_IMAGE_BUDGET` | 5,000,000 | the most `--image-budget-kb` may raise it to |
| `JPEG_QSCALE` | 3 | the JPEG quantizer a look starts at |
| `JPEG_QSCALE_MAX` | 12 | the coarsest it goes to fit |
| `RAW_STREAMS` | 4 | video streams `raw` draws side by side |
| `BUCKETS` | 50 | `analyze`'s buckets by default |
| `MAX_BUCKETS` | 200 | the most buckets |
| `ANALYZE_RESERVE` | 1.5 s | how long before its deadline `analyze` stops to answer |
| `TIMING_PACKETS` | 1,048,576 | packets `timing` holds: 4.8 hours at 60 fps |
| `LISTED` | 20 | times a list in `analyze`'s summary gives |
| `MAX_PIXELS` | 2^27 | the largest picture decoded (D24): over it is `too_large` |
| `EDITS_PER_CALL` | 1,000 | edits in one `edit`, one undo step |
| `LABEL_CHARS` | 200 | characters of an edit's label |
| `VIEW_KEYS` | 200 | keys a track in a `view` part, and markers or a target's keys in theirs |
| `VIEW_CHARS` | 30,000 | characters of keys in a `view` part |
| `HISTORY_STEPS` | 50 | the newest steps the `history` part gives |
| `SUMMARY_MARKERS` | 20 | markers the summary lists |
| `LOCK_WAIT` | 2 s | how long a call waits for another call's hold of a project; the window's is never waited for |
| `QUEUED_RENDERS` | 4 | renders waiting behind the one running; one more is `busy` |
| `FINISHED_JOBS` | 16 | finished jobs `job` reports on |
| `JOB_WAIT` | 45 s | the longest `job` `wait` |
| `JOB_WAIT_MARGIN` | 2 s | a wait ends this long before its call's deadline |
| `CANCEL_WAIT` | 2 s | how long `cancel` waits for the render and its file to go |
| `COMPARE_TIMES` | 24 | times in one `compare` |
| `COMPARE_COUNT` | 8 | times `compare` spreads by default |
| `SHEET_ROWS` | 4 | worst times `compare`'s `sheet` shows |
| `DIFF_GAIN` | 4 | how much the sheet brightens a difference |
| `COMPARE_RESERVE` | 1.5 s | how long before its deadline `compare` stops to answer, plus twice its slowest time |
| `MAX_TOOLS` | 12 | tools through M9 (A1 has 5, A2.2 adds 3, A2.4 2, A2.5 1) |
| `MAX_LIST_BYTES` | 12 KiB | the `tools/list` result, all tools |
| `A1_LIST_BYTES` | 7 KiB | A1's share of it |
| `INSTRUCTIONS_CHARS` | 1,500 | the server's instructions |

`analyze`'s thresholds, in the same module:

| Threshold | Value | What |
|---|---|---|
| `GAP_OVER` | 1.5 | a frame lasting more than this many nominal frames is a gap; round(duration / nominal) − 1 frames are missing |
| `VFR_SPREAD` | 1 % | durations spread past this share of the nominal, plus a tick, are a variable rate |
| `REPEAT_UNDER` | 5 % | a non-key packet under this share of the median non-key packet is a repeated picture |
| `MOTION_GRID` | 64 × 32 | the grid `motion` averages each frame's luma to |
| `CUT_OVER_MEDIAN` | 6 | a change this many times the median of the frames around it, and over `CUT_FLOOR`, is a cut |
| `CUT_FLOOR` | 12 | the least change (0 to 255) that is a cut |
| `CUT_WINDOW` | 15 | frames the median is taken over |
| `CLIP_LEVEL` | 0.999 | a sample at or past this, in a run of `CLIP_RUN`, is clipped |
| `CLIP_RUN` | 3 | samples in a row that are clipping |
| `SILENCE_DBFS` | −60 dBFS | quieter than this on every channel, as 10 ms RMS, is silent |
| `SILENCE_MIN` | 500 ms | the shortest silence |
| `DBFS_FLOOR` | −120 dBFS | the floor of every dBFS given |

## Schema version

The tools' arguments and results are schema version 1, which
`spherecut call` names `spherecut.call/1`. Within a version, fields are
only added, never renamed or given a new meaning, and a code keeps its
name and meaning (D27). A change that breaks that is a new version.

| Version | Date | Change |
|---|---|---|
| 1 | 2026-10-30 | A1: `probe`, `look`, `analyze`, `check`, `doctor` |
| 1 | 2026-10-31 | A2, fields and tools added: `project`, `edit`, `undo`, `render`, `job`, `compare`; `project` in `look` and `check` |
