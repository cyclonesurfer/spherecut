# Spherecut A1 (Look and check): the implementation plan

*2026-10-30, at A1's start, on main at 34f4560 plus D43. Written from
PLAN.md track A1, DECISIONS D23 and D43, `docs/research/agents-2026-10.md`,
docs/PROMPT.md and the code as M1 left it. Where this plan and PLAN.md
differ, PLAN.md wins until the difference is recorded there.*

## 0. What A1 is for

An agent (Claude Code, Codex, Gemini CLI, Copilot CLI, Cursor, Zed,
goose, opencode) starts `spherecut mcp` and can then go through a folder
of footage, look at any moment from any direction, find where something
happens, explain stutter or audio problems, and check a file's 360
metadata. Nothing in A1 changes or renders a file for keeps, apart from
an over-sized `look` image written where Pat allows writing.

PLAN's A1 criteria, and where this plan meets each:

| Criterion | Section |
|---|---|
| `spherecut mcp` serves 2025-06-18 and 2025-11-25, answers within 1 s | 3 |
| At most 12 tools, 12 KB of `tools/list`; A1's five | 4, 6 |
| Schemas: draft 2020-12, object root, no root combinators, snake_case, descriptions under 4,096, fixed order | 4.4 |
| Images: JPEG 768 px by default, capped at 1568 px and 24 tiles, sizes stated, PNG on request, larger to a file | 7 |
| Conventions: exact times, `{exact, seconds}` back, angles stated | 4.1 |
| Errors: `isError` results with code, argument, range, next step, under 500 characters | 4.2 |
| Safety: read-only by default, kernel-enforced roots, nothing overwritten, untrusted text, annotations, limits | 5, 4.3, 9 |
| One request and one response type per tool, shared with `spherecut call` | 2, 8 |
| Instructions, SKILL.md with three recipes, `agents install-skill`, `docs/agents.md` | 10 |
| Tier 1 tests with our own client, recipes on synthetic media within a budget | 11 |
| Tier 2: an MCP session decodes one of Pat's samples with `vaapi` | 11.4 |

## 1. Decisions

D43 settled D23's three open choices: our own synchronous MCP layer,
schemas written by hand, rustix for `openat2`. This plan adds these, to
be recorded in DECISIONS as their slices land:

- **D44, the MCP server**: the protocol layer (section 3), the tool
  conventions and errors (4), images (7), limits (9).
- **D45, roots**: how paths from an agent are opened and how media reads
  the file that was opened (5).
- **D46, analyze and check**: what each measures and the rules each
  applies, with their sources (6.3, 6.4).

None of them is expensive to reverse: the tools are a versioned public
interface (schemas in `docs/agents/schemas/`, changes listed in
`docs/agents.md`), and nothing stores data in their shape.

## 2. Crate layout

A new crate, **`crates/agent`** (`spherecut-agent`), `#![forbid(unsafe_code)]`:

```
crates/agent/src/
  lib.rs         Server, Options, the tool table
  rpc.rs         JSON-RPC framing, the reader, the writer, the dispatcher (section 3)
  session.rs     initialize, revisions, the state of one stdio session
  tools.rs       the tool list, schemas (include_str!), annotations, dispatch by name
  ops/mod.rs     Request and Response types per tool, shared with `call`
  ops/probe.rs   probe: files, folders, grouping, summaries, paging
  ops/look.rs    look: tiles, the sphere grid, the cube, the sheet
  ops/analyze.rs analyze: timing, motion, audio
  ops/check.rs   check: metadata rules and targets
  ops/doctor.rs  doctor with a file
  roots.rs       read and write roots, openat2 (section 5)
  image.rs       composing tiles, labels, JPEG through FFmpeg, PNG, base64
  font.rs        the 8x8 label font (public-domain data)
  text.rs        untrusted text: caps and replacements (4.3)
  limits.rs      every limit, in one place (section 9)
  error.rs       ToolError: code, argument, message, valid, next
  skill.rs       SKILL.md, compiled in, and install-skill
crates/agent/skill/spherecut/SKILL.md
docs/agents.md
docs/agents/schemas/{probe,look,analyze,check,doctor}.json
```

Dependencies: spherecut-core, -sphere, -media, -render; serde,
serde_json, thiserror, tracing; **rustix** `=1.1.6` with `std`, `fs` and
`stdio` (D43; in the lock already through tempfile). No other new crate.
JPEG goes through FFmpeg's `mjpeg` encoder in media (7.3), PNG through
the `image` crate the render crate already has.

`spherecut-cli` depends on it and gains three subcommands, each a thin
call into the crate:

- `spherecut mcp [--read-root DIR]... [--allow-write --write-root DIR]`
- `spherecut call <tool> '<json>' [--read-root DIR]... [--allow-write --write-root DIR] [--image-dir DIR]`
- `spherecut agents install-skill [--dry-run]`

**What moves.** `look` renders as `spherecut frame` does. Frame's steps 4
to 8 (probe, choose the stream, check it is equirect, the GPU, the
render node, the decoder, the exact seek, the render) move from
`crates/cli/src/frame.rs` into `spherecut-agent::still`
(`Still::open(source, opts)`, `Still::seek(time)`, `Still::render(view,
size)`), since the CLI depends on the agent crate and not the other way.
`frame.rs` keeps its flags, its output file, its SIGINT handling and its
JSON, so `frame`'s tests go on covering the path `look` takes. The move
is mechanical and comes first in slice A1.4, with frame's whole test
suite run before anything else changes.

## 3. The MCP layer (`rpc.rs`, `session.rs`)

### 3.1 Framing and threads

- **stdout is the protocol's alone.** At start, before anything else,
  the server duplicates file descriptor 1 for the protocol writer
  (`rustix::io::dup`) and points fd 1 at stderr
  (`rustix::stdio::dup2_stdout`), so that a stray write from FFmpeg, a
  driver or a dependency lands on stderr. Logs go to stderr through
  tracing, as in the CLI (D12).
- **Reading.** One reader thread reads stdin a line at a time, with
  `limits::MAX_REQUEST_BYTES` (1 MiB). A longer line is read to its end,
  dropped and answered with -32600 and a null id. Blank lines are
  skipped. A line that isn't JSON gets -32700 with a null id; one that
  is JSON but not a request, notification or response gets -32600. A
  JSON array (a batch, which 2025-06-18 removed) gets -32600. Ids must
  be strings or integers; anything else gets -32600 with a null id.
  Responses from the client (we never send requests) are ignored.
- **Writing.** One writer, behind a mutex, writes each message as one
  line of compact JSON and flushes it. `serde_json` escapes newlines
  inside strings, so a message never spans lines. A test parses every
  line the server writes (11.1).
- **Calls.** `initialize`, `ping`, `tools/list` and every notification
  are handled on the reader thread at once. `tools/call` goes to a pool
  of `limits::CALLS_AT_ONCE` (2) worker threads through a FIFO queue, so
  that a `ping` is answered while a `look` renders. Each call gets a
  `StopSource` (`core::cancel`) and a deadline of
  `limits::CALL_SECONDS` (50 s, under Codex's 60 s default).
- **Cancellation.** `notifications/cancelled` with a known `requestId`
  stops that call: a queued call is removed, a running one sees its
  token at its next check (FFmpeg's interrupt callback, between tiles,
  between buckets). Either way no response is sent for it. An unknown
  id is ignored (the call may have just finished).
- **Progress.** A call whose request has `_meta.progressToken` gets
  `notifications/progress` with an increasing `progress` and a `total`
  where one is known (tiles rendered, buckets done), at most every
  250 ms. Without a token, none.
- **stdin closing.** On EOF the reader stops every call, queued and
  running, waits up to 1.5 s for the workers, and exits 0. A worker
  stuck in a driver call past that is left behind: the process exits
  anyway (PLAN: within 2 s).
- **Errors in the server itself** (a panic in a tool) become an
  `isError` result with code `internal` for that call; the server goes
  on. A panic on the reader or writer thread exits with status 1 after
  a log line.

### 3.2 Revisions and the lifecycle

- `SUPPORTED = ["2025-11-25", "2025-06-18"]`.
- `initialize`: the server answers with the client's `protocolVersion`
  when it is one of `SUPPORTED`, else with `2025-11-25`, as the spec
  says. The result: `protocolVersion`, `capabilities: {tools:
  {listChanged: false}}`, `serverInfo: {name: "spherecut", title:
  "Spherecut", version}`, `instructions` (10.1). It is answered before
  FFmpeg, the GPU or doctor's checks are touched: the first tool call
  starts them (lazily, once per process, behind a `OnceLock`).
- Before `initialize`, every request but `ping` gets -32600 ("send
  initialize first"). A second `initialize` gets -32600.
  `notifications/initialized` is accepted and changes nothing: some
  clients send `tools/list` before it.
- **A modern request** (one whose `params._meta` has
  `io.modelcontextprotocol/protocolVersion`), `server/discover`
  included, gets -32022 with `data: {requested, supported: SUPPORTED}`.
  That is the fallback signal Claude Code's runtime waits for (D43).
- Unknown methods get -32601. `tools/call` naming an unknown tool gets
  -32602, as the spec says; one with bad arguments gets an `isError`
  result (4.2).
- `tools/list` takes and ignores `cursor`: the whole list fits in one
  page (4.4).

### 3.3 Results

A tool's result is one Response value (section 2's `ops`). The server
sends:

- `content[0]`: a text block with the Response as compact JSON. This is
  what every client shows the model.
- For `look`, an `image` block after it (base64, `image/jpeg` or
  `image/png`), unless the image went to a file.
- `structuredContent`: the same Response, **except when the result has
  an image**. Claude Code up to the fix for its issue 54737 dropped
  image blocks from results that also had `structuredContent`; the text
  block carries the same JSON, so leaving it out loses nothing.
- No `outputSchema` in A1. The response shapes are documented in
  `docs/agents.md` and versioned with the schemas.

`spherecut call` prints the same Response JSON on stdout, as one line,
and writes any image to a file named in it (section 8). A test runs
every tool both ways on the same input and compares (11.1).

## 4. Conventions

### 4.1 Times, angles, sizes

- **Times in** are exact: a string in `TimeSpec`'s grammar (`"12.5"`,
  `"1:02.5"`, `"01:02:03.250"`, `"30000/1001"`, `"370f"`) or a
  `[num, den]` pair of integers, seconds. A bare JSON number is refused
  with `bad_argument`, and the message gives the string to send instead
  (`12.5` → `"12.5"`). The schema says `"type": ["string", "array"]`;
  the description gives the grammar.
- **Times out** are `{exact: [num, den], seconds: f64}`, plus `frame`
  when the time is a frame of a video stream: `core::FrameTime`, the
  shape `probe --json` and `frame --json` already use (D27).
- **Angles** are degrees. yaw > 0 turns right, pitch > 0 looks up, roll
  > 0 rolls the camera clockwise as the viewer sees it (D33). yaw 0,
  pitch 0 looks at the centre of the equirect; yaw ±180 is its left and
  right edge; pitch 90 is its top row. `fov` is horizontal unless
  `vfov` is given (then `fov` is derived from the tile's aspect). Every
  tool description that takes a view says this.
- **Sizes** are pixels, `[width, height]` in results.

### 4.2 Errors

A failed call is a tool result with `isError: true` and one text block:

```json
{"error":{"code":"out_of_range","argument":"times[3]","message":"is after the clip's last frame","valid":"0 to 31.965 s (959 frames)","next":"use a time up to \"31.965\", or \"958f\""}}
```

- `code` is stable and listed in `docs/agents.md`; `argument` is a JSON
  path into the arguments (`file`, `times[3]`, `view.fov`), or null;
  `message`, `valid` and `next` are our own words. The whole is under
  `limits::ERROR_CHARS` (500), checked by a test over every code.
- **No text from media appears in an error**: not a file name, a tag,
  or an FFmpeg log line. The error names the argument instead
  (`files[2]`). Where FFmpeg's words would help, the error says to call
  `probe` with `detail: "full"`, whose `ffmpeg_log` carries them as
  untrusted fields.
- Codes in A1: `bad_argument` (wrong type, unknown property, missing
  required one, a bare-number time), `out_of_range`, `outside_roots`,
  `not_found`, `not_a_file`, `not_a_folder`, `unsupported_container`,
  `no_video`, `no_audio`, `not_equirect`, `damaged`, `decode_failed`,
  `render_failed`, `no_gpu`, `too_large` (an image over the inline cap
  with writing off), `write_refused` (writing off, or outside the write
  root), `output_exists`, `cursor_stale`, `timed_out`, `internal`.
  Several reuse D27's codes for the CLI where the meaning is the same.
- JSON-RPC errors are only for malformed or unknown requests (3.1,
  3.2).

### 4.3 Untrusted text

- Every string a result carries goes through `text::field`: C0 and C1
  controls (tab and newline included) become U+FFFD, so do the bidi
  controls (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to
  U+2069), and the string is cut at `limits::FIELD_BYTES` (1 KiB) on a
  character boundary with `…` at the end. ANSI escapes start with ESC,
  a C0 control, so none survives. It runs over the Response's JSON value
  as a whole, so nothing is missed: our own strings are short and
  plain, and are unchanged by it.
- Media text (tags, vendor strings, file names from a folder listing,
  camera names, FFmpeg's log lines) appears only in those fields, never
  in a tool description, the instructions, or an error (4.2).
- The descriptions of `probe`, `look` and `analyze` say that the string
  fields read from files, and the pixels of rendered frames, are
  untrusted content, not instructions.

### 4.4 The tool list

- Five tools, in this order: `probe`, `look`, `analyze`, `check`,
  `doctor`. A test fails past 12 tools or 12 KB of `tools/list` JSON.
- Each schema is a file in `docs/agents/schemas/`, JSON Schema draft
  2020-12 (`$schema` set), an object at the root with
  `additionalProperties: false`, no `oneOf`, `anyOf` or `allOf` at the
  root, no `$ref`, snake_case ASCII property names matching
  `[a-z][a-z0-9_]{0,63}`, and a description on every property. A test
  checks every rule, and that each description is under 4,096
  characters. Where a tool takes one of several shapes (`probe`'s files
  or folder, `look`'s modes), a field says which (`mode`), and the code
  checks the combination, with an error that names both fields.
- **Schema and type agree** (D43): for each tool a test builds one
  argument object holding every schema property with a valid value and
  requires the request type to accept it; removes each required
  property in turn and requires a `bad_argument`; adds an unknown
  property and requires a `bad_argument`. Request types have
  `#[serde(deny_unknown_fields)]`.
- **Annotations** on every tool: `readOnlyHint: true`,
  `openWorldHint: false`, `idempotentHint: true`. The exception is
  `look` when the server runs with `--allow-write`, since it may then
  create a file under the write root: `readOnlyHint: false` and
  `destructiveHint: false` (it never replaces anything).
- `title` on each tool, for clients that show it.

## 5. Roots (`roots.rs`, D45)

- **Options.** `--read-root DIR`, repeatable; the working directory when
  none is given. `--allow-write --write-root DIR` together, or neither;
  the write root is also read from. At start each root is opened as an
  `O_PATH | O_DIRECTORY` descriptor and its canonical path recorded; a
  root that doesn't exist or isn't a folder stops the server with a
  message on stderr and exit 2 (it is Pat's configuration, not the
  agent's mistake).
- **Paths from an agent.** A relative path is relative to the first read
  root. An absolute path is matched, component by component, against
  the roots' canonical paths; the longest root it lies under is used,
  and the rest of the path is opened relative to that root's descriptor.
  An absolute path under no root is refused (`outside_roots`, with the
  roots listed in `valid`). Matching is lexical only: the kernel does
  the resolving.
- **Opening.** Every open is `openat2(root, rest, flags,
  RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`. Inputs: `O_RDONLY |
  O_CLOEXEC | O_NOCTTY | O_NONBLOCK`, then `fstat` must say regular file
  (`not_a_file` otherwise, which a FIFO or device gets without blocking),
  then `O_NONBLOCK` is cleared. Folders: `O_RDONLY | O_DIRECTORY`, read
  with `rustix::fs::Dir`. Outputs: `O_WRONLY | O_CREAT | O_EXCL |
  O_CLOEXEC`, mode 0644, in the write root only, so an existing name,
  a symlink at the name included, is never followed or replaced
  (`output_exists`).
- **The kernel's answers**, as the prototypes saw them (D43): `EXDEV`
  for `..` out of the root, an absolute path, a symlink out and
  `/proc/self/fd/N` (`outside_roots`); `ENOENT` for a missing file and a
  dangling link (`not_found`); `ELOOP`, `ENOTDIR` and `EACCES` map to
  `not_found`, `not_a_folder` and `not_found` with "can't be read".
  `ENOSYS` (a kernel before 5.6) stops the server at start: the roots
  can't be enforced, so it refuses to run.
- **Media reads the file that was opened.** FFmpeg, probe's box reader
  and the camera detection all take a path today. For an agent's file
  they are given `/proc/self/fd/N`, N being the descriptor `openat2`
  returned, held open for the whole call. Opening that path follows the
  kernel's link to the open file itself, not to a name in the folder,
  so a file renamed, replaced or swapped for a symlink after the check
  is not what gets read: what was opened is. FFmpeg's confinement (D24)
  applies unchanged on top. The report's `path` and every message use
  the agent's path, not the `/proc` one. A test swaps the file after
  the open and checks that the first file's content is read (11.1).
  *Rejected:* giving FFmpeg a custom `AVIOContext` over the descriptor,
  and the box reader positional reads, which would be cleaner but
  changes every `MediaBackend` signature, the fakes and the GUI, for
  the same guarantee. *Revisit if* `/proc` can't be relied on (a
  sandbox without it), or M2's projects open media through the same
  path.
- **What the roots don't do**, which `docs/agents.md` says next to the
  network note: they limit what Spherecut opens for an agent, not what
  the agent can do. An agent that can run commands can read Pat's
  files without Spherecut; the roots only keep Spherecut from being a
  way around the agent's own sandbox.

## 6. The tools (`ops`)

Each tool has `ops::<tool>::Request` (serde, `deny_unknown_fields`) and
`ops::<tool>::Response` (serde). `ops::run(tool, value, &Context) ->
Result<Response, ToolError>` is what both `mcp` and `call` use. Context
holds the roots, the stop token, the deadline, the progress sink, and
the lazily made GPU and doctor checks.

### 6.1 `probe`

Request: `files` (1 to `limits::FILES_PER_CALL`, 64) or `folder` (one
path); `detail`: `summary` (default) or `full`; for a folder,
`page_size` (1 to 50, default 20) and `cursor`.

- **A file**: its summary, about 300 bytes, built from `ProbeReport`:
  `file`, `ok`, `duration` (time), `size` ([w, h] of the first video
  stream, or per lens), `fps` (number, with `vfr: true` when variable),
  `codec`, `bit_depth`, `projection` (`equirect`, `cubemap`,
  `dual_fisheye`, `flat`, `unknown`) with `metadata` (`spherical_v2`,
  `spherical_v1`, `vendor`, `none`), `camera` (the guess's model), `audio`
  (`"stereo, 48 kHz"`, `"ambiX 1st order, 4 channels"`, null),
  `problems` (codes only: `truncated`, `damaged`, warnings' codes).
  `detail: "full"` gives the whole `ProbeReport`, as `probe --json`
  gives it, through 4.3.
- **A folder**: one level, no recursion. Entries are read through the
  folder's descriptor; subfolders are listed by name in `folders` so
  the agent can go down. Video files are grouped into clips by the rules
  in `docs/formats/` (Insta360: `(VID|LRV)_date_time_LL_seq` with lens
  `_00_` and `_10_` and the proxy `_01_` or `_11_`; GoPro: `GSccnnnn`
  and `GXccnnnn` chapters `cc` of recording `nnnn`, with the `GL` proxy;
  DJI: an `.OSV` and the `.LRF` of the same stem; anything else, one file
  a clip). Clips are sorted by recording time: the name's when it has
  one, else the file's modification time, so that sorting costs no
  probe. Only the page's clips are probed. A clip's summary adds
  `files` (its files, by role: `lens0`, `lens1`, `proxy`, `chapters`)
  and `recorded`. The response has `clips`, `folders`, `skipped` (files
  that aren't video, counted), `total` (clips), and `next_cursor` when
  more remain.
- **The cursor** is `"<offset>:<fingerprint>"`, the fingerprint a hash
  of the folder's sorted names: a folder that changed between pages
  gets `cursor_stale`, with "start again without `cursor`" as `next`.
  Nothing is kept between calls.

### 6.2 `look`

Request: `file`; `mode`: `view` (default), `sphere`, `cube`, `sheet` or
`raw`; `times` (1 to 24) or `span` (`{from, to, count}`, count 2 to 24,
evenly spaced, both ends included); `view` (`yaw`, `pitch`, `roll`,
`fov`, `vfov`, `projection`: `rectilinear`, `fisheye` or
`stereographic`), or `views` (1 to 24, for one time); `size` (the long
edge of the whole image: 64 to 1568 inline, default 768); `format`:
`jpeg` (default) or `png`; `seek`: `exact` (default) or `keyframe`;
`output` (a new file under the write root, for an image past the inline
cap; only with `--allow-write`); `assume_equirect` (as `frame
--equirect`).

- **Tiles.** `view`: one tile per time and view (times × views ≤ 24).
  `sphere`: the whole equirect per time, with a yaw/pitch grid every 30°
  and its labels drawn over it. `cube`: the six directions (front,
  right, back, left, up, down at 90°, D33's axes) per time, so at most
  4 times. `sheet`: one small tile per time (2 to 24), the equirect with
  a light grid, or `view` when given. `raw`: the decoded picture as it
  is, every video track of the file side by side (an X5 `.insv`'s two
  lenses), for sources that aren't equirect yet. `view`, `sphere` and
  `cube` on a source that isn't equirect fail with `not_equirect`, and
  `next` names the clip's proxy when the folder has one, or `raw`.
- **Layout.** Tiles of one shape in a grid whose column count keeps the
  whole image's aspect closest to 16:9 (2:1 for `sphere`), each tile
  with a label band above it: line 1 the frame's time (`12.512 s  f 375`),
  line 2 the view (`yaw 90  pitch -20  fov 100`). The font is 8×8 pixels
  at scale 1, or 2 when a tile is 320 pixels wide or more; labels are
  white on near-black, outside the picture, so they never hide what's in
  it.
- **Rendering.** Each tile goes through `still.rs` (section 2): the
  decoder picked as the viewer picks it (doctor's checks, D34; the copy
  path, not zero copy: one frame a tile doesn't gain from it), an exact
  seek per time (times sorted first, so a file is read forwards), and
  `render_still` at the tile's size on the GPU (`Gpu::headless`, made
  once a process; lavapipe where there's no GPU, `no_gpu` where there
  isn't even that). `raw` tiles scale the planes on the CPU.
- **The response** lists every tile: `index`, `row`, `column`, `time`
  (the frame shown: `{exact, seconds, frame}`), `requested` (the time
  asked for), `view` (as rendered, `fov` and `vfov` both), `decode`
  (`vaapi`, `vulkan` or `software`, with `fallback` when hardware went
  to software). Then `image`: `width`, `height`, `format`, `bytes`, and
  `path` when written to a file. So a client that drops images still
  knows what the image held.
- **Time limit.** Past `limits::CALL_SECONDS` the call stops with
  `timed_out`, saying how many tiles were done and suggesting `seek:
  "keyframe"` or fewer times.

### 6.3 `analyze` (D46)

Request: `file`; `kind`: `timing`, `motion` or `audio`; `from`, `to`
(times, default the whole clip); `buckets` (1 to 200, default 50);
`stream` (default: the first video stream, or the first audio stream
for `audio`).

- **timing** reads packets only, with no decode: per bucket, frames,
  frame duration (min, mean, max ms), duplicate timestamps, gaps (a
  duration over 1.5× the nominal one, counted as the frames missing),
  keyframes, and the bitrate (Mb/s). Overall: the nominal and the
  measured rate, `vfr` (durations varying by more than 1 % of the
  nominal), the keyframe interval (median and longest), totals, and
  `repeated_pictures`: non-key packets under 2 % of the median non-key
  packet's size, which is what an encoder writes for a picture that
  repeats the last one. Thresholds are in `limits.rs` (they are not
  limits, but one place keeps them findable) and in the description.
- **motion** decodes the range (the viewer's decoder choice), reduces
  each frame's luma to 64×32 by area averaging, and gives per bucket the
  mean and largest mean absolute difference between consecutive frames
  (0 to 255), plus `cuts`: frames whose difference is over 6× the
  median of the 15 frames around it and over 12. When the deadline
  would pass, it stops at a frame boundary and returns what it measured
  with `complete: false` and `next_from`, the time to call again from.
- **audio** decodes the range (`media::audio`, the stream's own rate,
  each channel): per bucket, peak and RMS in dBFS (the loudest
  channel), `clipped` (samples at ±0.999 or beyond, in runs of 3 or
  more), and overall `silences` (runs of 0.5 s or longer below −60 dBFS
  RMS) as time ranges.
- **The response**: `kind`, `stream`, `from`, `to`, `bucket` (its
  duration), `buckets` (each with `from` and the measures), `summary`
  (the overall figures), `complete`, and for motion `next_from`. A
  bucket is about 120 bytes of JSON, so 200 buckets stay under 25 KB.

### 6.4 `check` (D46)

Request: `file`; `target` (optional): `youtube_360`, `flat_16_9`,
`vertical`, `archive`.

Rules, each `{id, ok, message, fix}` with `fix` only when `ok` is
false, and a top-level `pass`:

- **Always**: the spherical metadata agrees with the streams (an
  equirect is 2:1 mono, 1:1 top-bottom, 4:1 left-right; projection
  bounds inside the picture; one consistent stereo mode across v1 and
  v2 where both are present), spatial audio agrees with its channels
  (ambiX order n has (n+1)² channels, plus 2 for head-locked stereo; the
  SA3D box says ACN and SN3D), and the file isn't damaged.
- **`youtube_360`**: spherical metadata present and equirect (or
  cubemap where YouTube takes it), a codec, frame rate and size YouTube
  takes for 360, and for spatial audio first-order ambiX in 4 channels
  with SA3D. The exact list comes from YouTube's help pages, fetched
  when slice A1.6 is written and cited in D46 with the date; until then
  this plan names no numbers.
- **`flat_16_9`** and **`vertical`**: no spherical metadata (a flat
  video tagged spherical plays wrong), the aspect 16:9 or 9:16 within
  1 %, square pixels.
- **`archive`**: the camera's original tracks and metadata kept (for
  an `.insv`, its trailer; for any file, every stream FFmpeg found),
  nothing truncated.
- The fixes name what to do in Spherecut's terms where it can (M2's
  export), else in plain words; never a shell command.
- The response is under `limits::CHECK_BYTES` (2 KB), which a test
  checks on the worst synthetic file.

### 6.5 `doctor`

Request: `file` (optional), `speed` (boolean, default false),
`render_node` (optional, a path under `/dev/dri`, not under the roots).

- Without a file: `media::doctor::run`'s report, as `doctor --json`
  gives it.
- With a file: also `file`: the decode path that file gets (`vaapi`,
  `vulkan` or `software`) and, when hardware refused it, why (the
  fallback's code and reason, our own words). With `speed: true`, it
  decodes up to 5 s of the file (or the deadline's share) on that path
  and gives `fps` and `realtime` (measured fps over the stream's).

## 7. Images (`image.rs`)

### 7.1 Sizes

- The long edge of the whole image: default `limits::IMAGE_DEFAULT_EDGE`
  (768), at most `limits::IMAGE_MAX_EDGE` (1568) inline, at most
  `limits::MAX_TILES` (24) tiles. Tiles get the size that fits: a sheet
  of 24 at 768 is 6×4 tiles of 128×64.
- Past 1568, the image is written to `output` under the write root and
  the response gives its `path`; without `--allow-write`, `too_large`,
  with the limit and the flag named.
- An inline image also stays under `limits::IMAGE_MAX_BYTES` (3.75 MB,
  5 MB in base64, Bedrock's limit and half the API's): JPEG at 1568 px
  is far under it; a PNG over it is refused with `too_large`, suggesting
  JPEG.

### 7.2 Labels (`font.rs`)

The 8×8 font is the IBM PC BIOS glyph set as published in the public
domain by Daniel Hepper (`font8x8_basic.h`), ASCII 32 to 126 plus a
degree sign drawn the same way, kept as Rust data with its notice. It
needs no font file at run time and no new crate. A test renders every
glyph and checks each has ink and stays inside its cell.

### 7.3 Encoding

- **JPEG** through FFmpeg's `mjpeg` encoder, which every FFmpeg build
  has (native, unpatented; in Fedora's `ffmpeg-free` and in our compat
  builds): RGB from the composed image to `yuvj420p` through swscale,
  quality from `limits::JPEG_QSCALE` (3). A new function in media's
  `encode.rs`, `encode_jpeg(rgb, width, height) -> Vec<u8>`. A test
  decodes the JPEG back through FFmpeg and compares it with the input
  (PSNR over 35 dB on the grid clip's tile).
- **PNG** through `render::Image::to_png`, as `frame`.
- **base64** written here (20 lines, RFC 4648 with padding), tested
  against the RFC's vectors: no crate for it.

## 8. `spherecut call` and `agents install-skill`

- `spherecut call <tool> '<json>'` parses the JSON, runs `ops::run` with
  the same roots and limits as `mcp`, and prints the Response as one
  JSON line on stdout, exit 0; a tool error prints the same `{"error":
  …}` object and exits 1. An image goes to a new file in `--image-dir`
  (default the working directory; created with `O_EXCL`), named
  `spherecut-<tool>-<millis>-<n>.<ext>`, and its `path` is in the
  Response. Usage errors exit 2 with D27's usage document.
- `spherecut agents install-skill` writes the compiled-in skill to
  `$XDG_DATA_HOME/spherecut/skills/spherecut/SKILL.md` (default
  `~/.local/share`), then links `~/.claude/skills/spherecut` and
  `~/.agents/skills/spherecut` to that folder, creating the parents. An
  existing link to the same place is left; anything else at either
  name is left alone and reported, exit 1. `--dry-run` says what it
  would do. Nothing else touches those folders.

## 9. Limits (`limits.rs`)

Every limit is a named constant with a doc comment, listed in
`docs/agents.md` by a test that reads the module, and each has its own
test:

| Limit | Value | Test |
|---|---|---|
| `MAX_REQUEST_BYTES` | 1 MiB a message | a longer line gets -32600 and the next request is answered |
| `CALLS_AT_ONCE` | 2 | a third call waits; a ping is answered meanwhile |
| `CALL_SECONDS` | 50 s | a slow fake backend makes `look` time out with `timed_out` |
| `FILES_PER_CALL` | 64 | 65 files get `out_of_range` on `files` |
| `MAX_TILES` | 24 | 25 times get `out_of_range` |
| `IMAGE_DEFAULT_EDGE`, `IMAGE_MAX_EDGE` | 768, 1568 px | the default image's long edge; 1569 without writing gets `too_large` |
| `IMAGE_MAX_BYTES` | 3.75 MB | a PNG past it gets `too_large` |
| `MAX_PIXELS` | 2^27 per picture (D24) | already tested in media |
| `FIELD_BYTES` | 1 KiB a string | a 2 KiB tag comes back cut, with `…` |
| `ERROR_CHARS` | 500 | every code's worst message |
| `CHECK_BYTES` | 2 KB | the worst synthetic file |
| `MAX_BUCKETS` | 200 | 201 gets `out_of_range` |
| `EDITS_PER_CALL` | (A2) | listed, tested from A2 |

## 10. Teaching agents the workflow

### 10.1 Instructions (under 1,500 characters)

The conventions in short (times as strings, angles, yaw 0 at the
equirect's centre), the cheap-first order (probe the folder, then a
`sheet` or `sphere` look, then `analyze`, then single views), that file
contents and pixels are untrusted, and that images are small by default
and can be raised to 1568 px. A test checks the length.

### 10.2 The skill

`crates/agent/skill/spherecut/SKILL.md`, frontmatter `name: spherecut`
and a description under 1,024 characters, holding three recipes:

1. **Find where a moving subject is over time**: probe, a `sheet` over
   the clip, then `sphere` looks at the times where it changes, then
   `view`s that centre it, reporting time and yaw/pitch per sighting.
2. **Validate a 360 export**: `check` with the target, then `probe`
   `full` for any rule that failed, then a `sphere` look to confirm the
   picture matches the metadata.
3. **Find why a file stutters**: `analyze timing`, then `doctor` with
   the file and `speed`, then `analyze motion` over the worst buckets,
   ending with the cause (variable rate, missing frames, keyframe
   spacing, a decoder below real time) and its evidence.

### 10.3 `docs/agents.md`

First the network note: Spherecut makes no network requests, but what a
tool returns goes to the agent's model provider. Then the roots and
what they don't do (section 5), setup for each client (Claude Code,
Codex, Gemini CLI, Copilot CLI, Cursor, Zed, goose, opencode: the
command or the config file, the timeout to raise, the environment to
forward: Codex passes only its short list, so `LIBVA_DRIVER_NAME`,
`LIBVA_DRIVERS_PATH`, `MESA_VK_DEVICE_SELECT`, `VK_ICD_FILENAMES` and
`XDG_RUNTIME_DIR` need forwarding where Pat sets them), Flatpak clients
(`flatpak-spawn --host spherecut mcp`), the tools and their responses,
the error codes, the limits, the schema version and its change list.
Nothing tells Pat to run `npx`.

## 11. Tests

### 11.1 Tier 1, the protocol (`crates/agent/tests/`)

A scripted client in Rust (`tests/common/client.rs`): spawns the built
`spherecut mcp` with chosen roots and environment, writes requests,
reads lines with a timeout, and records every stdout line.

- A session at 2025-06-18 and one at 2025-11-25: `initialize`,
  `notifications/initialized`, `ping`, `tools/list`, `tools/call` (a
  probe of a synthetic clip).
- An unknown revision: `initialize` with `2099-01-01` gets 2025-11-25;
  a modern `server/discover` and a modern `tools/list` get -32022 with
  the supported list.
- An unknown tool (-32602), an unknown method (-32601), a request
  before `initialize` (-32600), bad arguments (an `isError` result with
  `bad_argument` and the argument's path), a bare-number time (its
  message gives the string).
- Malformed JSON (-32700, null id, the next request answered), a batch
  (-32600), a line over the limit (-32600, the next request answered).
- Cancellation: a `look` on a slow fake backend, cancelled; no response
  for its id, the worker stops within 1 s (a log line says when), and a
  `ping` sent after is answered.
- stdin closing with a call in flight: the server exits within 2 s.
- Every stdout line, over all these sessions, parses as one JSON-RPC
  message; a deliberate `println!` from a test-only tool reaches stderr,
  not stdout (3.1's guard).
- A run with the environment cut to Codex's list (`HOME`, `LOGNAME`,
  `PATH`, `SHELL`, `USER`, `LANG`, `LC_ALL`, `TERM`, `TMPDIR`, `TZ`)
  still lists tools and probes a file.
- Startup: `initialize` answered within 1 s of spawn, and before any
  FFmpeg or GPU set-up (a log line marks each; the test checks the
  order).
- The JSON-RPC reader fuzzed with proptest: arbitrary bytes and
  arbitrary JSON values never panic it, every non-empty line gets
  exactly zero or one response, and ids round-trip.
- `call` and `mcp` give identical JSON for every tool on the same input,
  images apart.
- Roots: `..`, an absolute path outside, a symlink out, a dangling
  symlink, `/proc/self/fd/N`, and a file swapped for a symlink between
  two calls, each refused with its code; a FIFO is refused without
  blocking; a file swapped after the open is still the one read (5).
- Writing: off by default (`write_refused`); with it, a new file only,
  an existing name refused, a symlink at the name refused.
- The schema rules and schema–type agreement (4.4), the tool count and
  `tools/list` size, the instructions' length, every error under 500
  characters, the untrusted-text rules on a synthetic file whose tags
  and name carry controls, bidi characters, ANSI and 2 KiB of text.

### 11.2 Tier 1, the tools

Each tool's behaviour on synthetic media (`spherecut-synth` and
`gen-media`): probe's summary fields and folder grouping on a synthetic
camera folder (empty files with Insta360, GoPro and DJI names beside
real synthetic clips), paging and `cursor_stale`; look's tile times and
views, the labels' place, the sphere grid's lines at the yaw and pitch
they label (checked on the grid clip, D9), cube faces, raw tiles,
`not_equirect`'s `next`; analyze's timing on a clip with planted jitter
and repeated pictures (a new `gen-media` clip, below), motion's cuts on
a clip with planted cuts, audio's peak, clipping and silence on the
synthetic tone with planted runs; check's rules on clips with and
without metadata, a 2:1 file tagged 1:1, ambiX with the wrong channel
count; doctor with a file in software.

`gen-media` adds `jitter_dups.mp4`: 30 fps H.264 whose timestamps jitter
by ±20 % for a second, with two gaps of 3 frames and 10 repeated
pictures at known frames. The grid clips' moving marker (D9) serves the
first recipe.

### 11.3 The recipes, replayed

A test plays each recipe as a fixed script of calls (what an agent
following SKILL.md would send) on synthetic media and requires the
planted answer: the marker's yaw at three times within 5°, the failed
rule on a clip with a wrong spherical box, the planted jitter and gaps.
Each script's cost is counted as text characters / 4 plus ceil(w/28) ×
ceil(h/28) per image, and must stay under its stated budget (recipe 1:
12,000; recipe 2: 4,000; recipe 3: 8,000).

### 11.4 Tier 2

`mcp_session_decodes_with_vaapi` in `crates/agent/tests/tier2_mcp.rs`:
an MCP session on one of Pat's samples (the X5 LRV), `doctor` with the
file and a `look`, requiring `vaapi` in both where doctor says VA-API
works; without hardware it skips with the reason and never passes.
`cargo xtask hw-test` picks it up. Pat runs the recipes on the samples
with two agents later (PLAN), and PROGRESS records the clients, the
revision each negotiated, the calls and the output sizes.

## 12. Slices

Each slice ends with `cargo xtask check` passing and its own tests; the
last ends with the full check, compat, a fresh-subagent review against
PLAN's A1 criteria, and the note to Pat.

1. **A1.1 The crate and the protocol.** `crates/agent` with `rpc`,
   `session`, `tools` (with a stub `doctor` only), `limits`, `text`,
   `error`; `spherecut mcp`; the stdout guard; the client and every
   protocol test in 11.1 that needs no media; the fuzz test. D44's
   protocol half.
2. **A1.2 Roots.** `roots.rs` and its tests; `/proc/self/fd` handed to
   media; `--read-root`, `--allow-write`, `--write-root`. D45.
3. **A1.3 probe and doctor, and `call`.** The summary, the folder
   grouping and paging, doctor with a file and speed; `spherecut call`;
   the mcp/call identity test.
4. **A1.4 look.** frame's core moved to `still.rs` first (frame's tests
   green before and after); then tiles, modes, labels, the font, JPEG in
   media, base64, the size caps, `output`; look's tests; D44's images.
5. **A1.5 analyze.** Packet timing in media, motion, audio,
   `jitter_dups.mp4`; analyze's tests; D46's analyze half.
6. **A1.6 check.** YouTube's requirements fetched and cited; the rules;
   check's tests; D46.
7. **A1.7 Teaching and finishing.** Instructions, SKILL.md,
   `install-skill`, `docs/agents.md`, the recipes replayed, the Tier 2
   test and its hw-test pickup, ARCHITECTURE, PROGRESS, PLAN's ticks,
   README; the review; the note to Pat.

## 13. Risks

- **`/proc/self/fd`** (5) is the one place this plan relies on procfs.
  Every desktop and Flatpak has it; a test fails loudly where it's
  missing rather than falling back to the path.
- **look's speed** on 8K software decoding: 24 exact seeks may pass
  50 s. The time limit, `seek: "keyframe"` and the proxy in
  `not_equirect`'s and `timed_out`'s `next` are the answers; the Tier 2
  run measures it.
- **Clients' handling of images and schemas** moves fast (D23's
  research). PLAN's hardware criterion has Pat run the recipes on two
  real clients; what they show goes into PROGRESS and, if needed,
  changes 3.3.
- **`frame`'s move** (section 2) touches a well-tested command; it goes
  first in A1.4 and alone, so a regression shows before look builds on
  it.
