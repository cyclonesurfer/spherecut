# Spherecut A2 (Edit and render): the implementation plan

Written 2026-10-31, after M2.9 (main 7179dbf). Read with PLAN's "A2 Edit
and render (after M2)", docs/design/a1-plan.md (A1's layer, roots,
conventions and limits, which A2 keeps), docs/design/m2-plan.md
sections 4 and 5 (the `Edit` values and the project file) and 8.6 (the
export job), DECISIONS D16, D43 to D48 and D51 to D55, and the code as
M2 left it.

## Amendments, 2026-10-31, after the fresh review (read first)

A fresh subagent reviewed this plan against PLAN and the code. Its
fourteen findings are taken as follows; the sections below are
corrected to match.

1. **A history matches its project by revision and by a hash of the
   project file's bytes** (3.3). Revisions are small counters that a
   hand edit leaves alone and that two projects share, so the number
   alone would let an undo apply to a changed or another project. In
   core since A2.1's first commit.
2. **The export never follows a link at its output** (5.3). It checked
   once and then followed links (`pipeline::folder`,
   `Mp4Writer::create_with`), so a link planted at the name mid-render
   would have led its files out of the write root. A2.4 adds an
   `ExportJob` mode that takes the output path exactly as given, with
   the free-space check and the sweep on that folder, so `link(2)`
   refuses anything that turns up at the name. Project saves never go
   through `file::save` with a `/proc` path.
3. **`tools/list` is budgeted per tool now** (4.5, new). Measured as
   `tools.rs` lists them, A1's five take 7,033 bytes; a sketch of the
   seven at half A1's density took 7,189, 2 KB over 12 KiB. So: one
   title per tool (the top-level one for 2025-06-18 and later, the
   annotation's for older clients, saving about 40 bytes a tool);
   `undo` and `redo` are one tool (`undo` with `redo: true`, about
   570 bytes); compare's `against` is flattened; and each tool has a
   byte budget its own test holds it to, in the slice that adds it. If
   they still don't fit, the 12 KiB is Pat's number to change, and they are
   asked rather than the budget quietly raised.
4. **The lock** (3.2): the lock file is never replaced (its holder line
   is written in place); a taker checks after `flock` that the file is
   still the one at the name; a lock file removed while held lets a
   second writer in, which the identity check (3.4) then keeps from
   saving over the first. The GUI shares the lock with a save that
   outlives its `Doc`, takes the target's lock for Save As and Replace,
   locks beside a link's target (where an agent locks), waits 2 s for
   an agent's call, and opens a project in a folder it can't write
   without a lock, saying it can't be saved there (6).
5. **`job`'s wait can't starve the server** (5.2): one `job` call waits
   at a time (another answers at once with the state), so a worker is
   always free of waits, and a wait ends 2 s before its call's deadline
   (`CallCx::remaining`). opencode's default limit is 30 s, so the docs
   say to wait less there.
6. **Media paths follow D47, not the kernel** (3.1): the folder's
   descriptor is for I/O only; paths are made relative to and read
   against the folder's path as named, lexically. An agent's folder is
   named by its root's given path joined with the agent's, and a new
   project's media is recorded the same way. The summary's media
   status goes through the roots, never `file::media_status`.
7. **The server's end** (5.4): jobs are cancelled when the calls are,
   against one deadline. `EXIT_GRACE` (1.5 s) can be shorter than a
   cancel on a slow device, and then the temporary file is left for the
   next render's sweep; `SWEEP_AGE` drops from 60 s to 10 s in A2.4 (the
   age only guards the moment between a file's creation and its
   `flock`), and 1.1 states the reading.
8. **Error codes** (10): every code `render` and `job` pass through from
   the export is listed (D51's `cannot_export`, `no_encoder`,
   `encode_failed`, `mux_failed`, `metadata_failed`, `check_failed`,
   `io_error` and the hardware codes). A project file's load errors map
   to `not_found`, `not_project` (not JSON, not a project, over 64 MiB),
   `newer_schema`, `bad_project` (malformed or breaking a rule) and
   `io_error`, never D27's `too_large`. `cancelled` becomes a job state
   as well as the code an agent never sees, and `busy` covers a full
   render queue too; both descriptions say so.
9. **Queued renders reserve their output names** (5.1), so a second
   render to the same name is refused when it is asked for, not after
   the first has finished.
10. **Both files are written before either is renamed**, the project's
    rename first (3.3), so a failed write leaves both as they were and
    only a crash between the two renames leaves a stale history. A
    trimmed history keeps the newest steps, or none.
11. **A redo doesn't move its step's revisions.** The undo rule uses the
    history's match with its project, not the steps' numbers.
12. **`add_clip` and `remove_clip` can't pass under M2** (1.1): the
    project is validated after each list of edits, and M2 holds one
    clip. The every-variant test checks they are refused with `limit`.
13. **Frames in times** (4.1) are counted at the rate of the time's own
    axis: a clip's keys, markers and trim at its media's rate, a
    target's path at its media's, `add_clip`'s `at` at the sequence's.
    "Project times are file times" holds while a clip sits at 0 (M2).
    `add_media` takes only `path`.
14. **Small fixes**: section numbers in section 0's table; 9.5's label
    is the `call` client's ("spherecut call"); `project` isn't
    read-only with writing on; look's and check's new `project` goes in
    `docs/agents.md`'s list of schema changes; a project's name may take
    246 bytes, so its history's name fits in 255.

## 0. What A2 is for

An agent opens a project, changes its trim, keys, markers, targets,
output and export choices, undoes its own changes, renders, and checks
and compares the result, while the project isn't open in the GUI. Pat
then opens the project in the GUI and can undo the agent's steps with
Ctrl+Z, each labelled with the agent's name.

PLAN's A2 criteria, and where this plan meets each:

| Criterion | Section |
|---|---|
| `project`, `edit`, `undo`, `redo`, `render`, `job`, `compare`; project tools stateless, each call names the file; no save tool; the project under the write root | 3, 4 |
| `edit`: M2's `Edit` values, all or none, one undo step labelled with the agent's name; `dry_run`, `if_revision`; one edit sets a whole view path | 4.3 |
| History beside the project with each step's revision; the GUI reads and writes it; `undo`/`redo` with `if_revision`; an agent undoes only an agent's step on top (`not_top`, `stale_revision`) | 3.3, 4.4, 6 |
| An advisory lock to write; a project the GUI has open is read-only to `spherecut mcp`; a save refused if the file changed on disk since the call loaded it; atomic writes | 3.2, 3.4 |
| `render` returns a job id within 1 s; `job` with progress, `wait` and `cancel`; one at a time, 4 queued; output only when finished and validated; free-space refusal; `range` and draft; jobs cancelled with the server, `job_lost`, a killed server's file swept | 5 |
| `compare` (PSNR/SSIM per time, the worst, a difference sheet); `look` and `check` take a project | 7 |
| A test applies one of every `Edit` variant through MCP; GUI actions that aren't edits listed in `docs/agents.md` | 9, 8.3 |

## 1. Decisions

To be recorded in DECISIONS as their slices land:

- **D56, projects for agents**: the project folder handle, the lock, the
  history file, the changed-on-disk check, the one exception to "nothing
  is replaced", times in edits, and the agent's undo rule (sections 3, 4,
  6).
- **D57, render jobs**: the job table, ids, the queue, cancel and the
  server's end, the output beneath the write root (5).
- **D58, compare, and projects in look and check** (7).

None is expensive to reverse. The history file and the lock file are new
files beside a project; a build that doesn't know them ignores them, and
a history that doesn't match its project is dropped, never trusted (3.3).

### 1.1 Where this plan reads PLAN differently

- **`job`'s `wait` is at most 45 s, not 50.** A call has 50 s from its
  request (`limits::CALL_TIME`), so a 50 s wait would end as `timed_out`.
  45 s leaves the call time to answer. Codex's 60 s default is still the
  shortest limit found.
- **The writing tools are listed only with a write root.** Without
  `--allow-write --write-root`, `tools/list` has A1's five, `project`
  (reading only) and `compare`; `edit`, `undo`, `render` and `job`
  appear with a write root. An agent with no way to write isn't
  shown four tools that would refuse every call, and its context keeps
  the 3 KB they take.
- **Times in a project are the clip's file time.** M2's keys, markers
  and trim are in the source's own time (0 at its first frame), which is
  the time probe, look and analyze give. Edits, `render`'s `range` and
  `project`'s parts use it, so an agent can key the moment look showed
  it. A render's own time starts at 0 at the trim's start; `compare`
  says how the two line up (7.1).
- **The agent's undo rule, made exact.** "Only when that step is on top
  and the project is still at its revision" is checked as: the step below
  the history's cursor is an agent's, and the history file was written
  with the project at the revision the project file holds and with the
  same bytes (3.3). Every
  writer that keeps the history (the GUI, `spherecut mcp`) writes both
  together, so a match means nothing changed the project since the
  history's top step but the steps in it; a mismatch means something
  else did (a hand edit, `git checkout`, an older build), and the undo is
  refused `stale_revision`. Comparing the project's revision with the
  step's own `after` would refuse an agent's second undo in a row, since
  every undo is a new revision (M2 plan 4.3).
- **`undo` and `redo` are one tool**, `undo` with `redo: true`, for
  `tools/list`'s 12 KiB (4.5). Eleven tools in all.
- **Not every `Edit` variant can be applied**: `add_clip` and
  `remove_clip` are refused under M2's one clip, as M2's limits say, so
  the every-variant test checks their refusal.
- **"Leave no files" when the server ends** holds when the export stops
  within `EXIT_GRACE`; when it can't (a slow device mid-frame), the
  temporary file it leaves is removed by the next render in that folder
  once it is 10 s old (5.4).
- **`spherecut call` refuses `render` and `job`.** `call` is one process
  per call, and a job ends with its server (5.4), so a render from
  `call` would be cancelled as it started. The error's `next` names
  `spherecut render`, which is the same export with `--json` progress.

## 2. Code layout

```
crates/core/src/project/
  folder.rs      ProjectFolder: a folder handle and its real path; load
                 with identity, save with history, the lock (section 3)
  store.rs       the history file's envelope, stale detection, its byte cap
crates/agent/src/
  ops/project.rs project: create, the summary, parts, the edit reference
  ops/edit.rs    edit and undo (with redo); the wire forms (4.1)
  ops/render.rs  render's checks and the job it queues
  ops/job.rs     job: progress, wait, cancel, the report
  ops/compare.rs compare
  jobs.rs        the job table, queue and worker (section 5)
  reference.rs   one example per Edit op, for project's "edits" part and the tests
docs/agents/schemas/{project,edit,undo,render,job,compare}.json
```

**What moves.**

- `doc::INITIAL_VIEW` (app) moves to `spherecut_core::project`, since
  `project` create makes the same untitled project the GUI does.
- `reframe::untitled_media` (app) moves to `spherecut_media` as
  `project_media(source, info)`, taking a `Source` so that the agent's
  copy reads through the file it opened (D45). The app passes a path
  source.
- `Tools` (rpc) gains `fn shutdown(&self, until: Instant) -> usize`,
  called by `serve` after it stops the calls, returning what it couldn't
  stop in time; `Ended.stuck` counts those too (5.4).
- `CallCx` gains `client(&self) -> &str`: the `clientInfo.name` from
  `initialize`, through `text::field` and cut to 64 characters, `agent`
  when there was none, `spherecut call` for `call`. It names the agent in
  each step's origin (4.2).

No new crate and no new dependency: rustix's `fs` feature is in core
already (D16's `TempFile`), and `flock` is in it.

## 3. Projects on disk

### 3.1 The folder handle

Every read and write of a project goes through a `ProjectFolder`: an
`O_PATH | O_DIRECTORY` descriptor for the folder the project is in, and
that folder's real path (the kernel's name for the descriptor, read from
`/proc/self/fd/N`).

- The agent gets the descriptor from its roots: beneath a read root (or
  the write root) to read, beneath the write root to write, by
  `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` as A1's outputs are
  (D45). A new `Roots::project_folder(arg, path, write)` returns the
  folder and the file's name.
- The GUI opens the folder by its path, with nothing beneath. Same code
  from there on.

Files in the folder are opened with `openat(fd, name, O_NOFOLLOW)`, so
the project, its history and its lock are what is in that folder, never
what a link there points to. A link at the project's name is refused,
for reading and writing alike (`bad_argument`: "a symbolic link; give
the path of the file it points to"), since a rename would replace the
link and leave its target as it was. The GUI follows a link at the
path it opens before it opens the folder, as D47 has it, so it locks
and writes beside the link's target, where an agent would.

Writing goes through `/proc/self/fd/N/<name>`: `TempFile` is made in
the folder the descriptor holds and renamed there (D16), whatever has
happened to the folder's path since. The descriptor is for I/O only.
Media paths are made relative to, and read against, the folder's path
as named, lexically (D47): the GUI's as Pat opened it, an agent's as
its root's given path joined with the agent's. So the two read a
project's `../clips/a.mp4` as the same file even where a folder on the
way is a symlink.

### 3.2 The lock

`.<name>.lock` beside the project (`.trip.spherecut.lock`), created
`0600` if it isn't there, never removed and never replaced, since a new
file would be a new lock. Writers take `flock(LOCK_EX | LOCK_NB)` on
it, then check that the file is still the one at the name (a lock file
removed or replaced just before is tried again). A lock file removed
while held lets the next writer in; the identity check (3.4) then keeps
either from saving over the other.

- **The GUI** takes it when it opens a project file, saves an untitled
  one, or saves as another file (the target's, before anything is
  written), and holds it until the project is closed; a save still
  running after a close holds it until it ends. An agent's call holding
  it is waited for, 2 s. Another GUI's open of the same file is refused
  with a notice ("open in another Spherecut window"). In a folder it
  can't write, the GUI opens the project without a lock and says it
  can't be saved there.
- **`spherecut mcp`** takes it for one call: `project` create, `edit`,
  or `undo`, from the load to the save. A lock held by another
  call is waited for, up to 2 s, tried every 20 ms; one held by the GUI
  is refused at once.

The holder writes one line into the file in place after it locks it:
`{"by": "gui" | "mcp", "pid": n}`. A refusal reads it to say who holds
the lock (`project_locked`: "open in Spherecut's window (pid 4121); until
agents can attach (A3), ask Pat to close it there, then try again"). A
line left by a holder that has gone is harmless: the lock is free then,
and the next holder writes its own.

Reading takes no lock: a project file is always whole (D16), so
`project`, `look`, `check`, `compare` and `render` read one the GUI has
open. `render` reads it once, when it is called.

### 3.3 The history file

`.<name>.history` beside the project, compact JSON:

```json
{"format": "spherecut.history", "schema": 1, "project_revision": 41,
 "project_hash": "8c3e0f2a91d4b7e5",
 "history": {"steps": [...], "cursor": 6, "limit": 1000}}
```

- **Written with the project**: both are written to temporary files
  and flushed, then the project is renamed into place, then the
  history. A failed write leaves both as they were; only a crash between
  the two renames leaves a history that doesn't match its project.
- **Read with its project**: when `project_revision` isn't the
  project's revision, or `project_hash` isn't the FNV-1a 64 of the
  project file's bytes, the history is stale. Then nothing in it is
  trusted: undo and redo are refused `stale_revision` (with the two
  revisions), and the next write starts a fresh history with this
  call's step, saying so (`history_reset: true`). The GUI opens such a
  project with a fresh history and a notice.
- **No history file** is an empty history, not a stale one: a project
  from M2, or one whose history was deleted.
- **At most 16 MiB.** The oldest steps go first until it fits
  (`History::keep_bytes`), and an undone step goes with every step
  before it, so what is saved is the newest steps, or none; the result
  says how many went. 1,000 steps (the limit) of small edits are well
  under 1 MiB; a 10,000-point view path, with the keys it replaced as
  its inverse, is a few MB.

### 3.4 Changed on disk

A load records the project file's identity: device, inode, size and
modification time in nanoseconds (`fstat` of the descriptor it read).
A save first takes the identity of what is at the name now
(`fstatat(fd, name, AT_SYMLINK_NOFOLLOW)`); if it differs, the save is
refused (`changed_on_disk`) and nothing is written. The lock keeps
Spherecut's writers apart, so this catches the others: a text editor,
`git checkout`, a sync tool, an older build. The window between the
check and the rename is a few microseconds and is stated, not closed.

The GUI keeps the identity of its last load or save and checks it the
same way: a project changed on disk is not saved over; the notice says
what happened and offers Save As.

### 3.5 What may be replaced

A1 never replaces a file. A2 replaces exactly two, and only beneath the
write root: a project file that loads as a project (so `edit` can't be
pointed at any other file), and its `.history` beside it. The `.lock`
is created if missing and written in place, never replaced.
`project` create makes a new file and refuses one that is there
(`output_exists`); `render`'s output is always new (5.3).

## 4. The project tools

All four load the named project, and all but `project` without `create`
save it in the same call. None keeps anything between calls.

### 4.1 Wire forms

Edits are M2's `Edit` values as the project file writes them (`{"op":
"set_trim", "clip": 4, "end": ...}`), with two allowances:

- **Times** may be given as A1's times: `"12.5"`, `"1:02.5"`,
  `"30000/1001"`, `"120f"`, or `[num, den]`, as well as the file's
  `{"exact": "25/2"}`. A pass before the typed read turns every value
  of a field named `at`, `start` or `end` inside an edit into the
  file's form. A frame count is at the rate of that time's axis: a
  clip's keys, markers and trim at its media's rate, a target's path at
  its media's, `add_clip`'s `at` at the sequence's. A test
  checks, over one example of every op, that every field with those
  names is a time and every time has one of those names, so a new field
  can't slip past the pass.
- **Ids** are integers, as `project` gives them; 0 or left out in
  something added is "give it a new id" (M2 plan 4.1).

Orientations are `{"ypr_deg": [yaw, pitch, roll]}` (D33's angles, as
look's), or `{"quat": [...]}`. Results give times as `{exact, seconds}`
and orientations with both forms, as A1's results do.

### 4.2 `project`

`{file, create?: {media}, part?, clip?, from?, to?}`

- **`create`** makes the untitled project of `media` (a path beneath
  the roots, probed through what was opened) at `file`, with
  `INITIAL_VIEW` and an empty history, and gives its summary.
- **No `part`** gives the summary: name, revision, rate, output, export
  settings, the trim, the media (path, size, video, and whether it is
  there and the same, `present`/`missing`/`changed`, or
  `outside_roots`), each track's key count with its first and last
  times, markers (the first 20), targets, the top of the history (its
  label, origin and whether an agent may undo it now, and why not), and
  `locked` with the holder when the GUI has it open. A few KB at most,
  whatever the project's size.
- **`part`** is one of `sequence`, `view` (a clip's keys, at most 200 a
  track from `from` to `to`, with `more` and the time to go on from),
  `markers`, `targets`, `history` (the last 50 steps) or `edits` (the
  edit reference: every op, its fields, and one example each, from
  `reference.rs`). `clip` names the clip, the only one by default.

### 4.3 `edit`

`{file, edits: [...], label?, dry_run?, if_revision?}`

- Applies `edits` with `History::record`, all or none, as one step:
  `label` (the agent's words, at most 200 characters; default the first
  op's, "Set trim", or "N edits") and `Origin::Agent { name }`
  (`CallCx::client`). The GUI's Edit menu shows "Undo Set trim
  (claude-code)".
- **`if_revision`**: the project's revision must be this one, else
  `stale_revision` with the current one, and the agent reads the project
  again.
- **`dry_run`** applies to a copy, saves nothing, and gives what would
  happen: the revision, the new ids, the warnings.
- The result: the new revision, the step's label, the ids given to
  added things (by the edit's index), `history_reset` when 3.3 dropped a
  stale history, and warnings that aren't refusals: a key past the
  trim's end, which no render shows; a marker outside it.
- A refused edit is `bad_edit` at `edits[i]`, with the op, the rule's
  code (M2's `Code`: `not_found`, `in_use`, `exists`, `last_key`,
  `out_of_range`, `invalid`, `limit`, `id_given`) and its message.
  Nothing is saved.
- `add_media` takes `{"op": "add_media", "path": "clips/b.mp4"}` from an
  agent, and only that: the op probes the file beneath the roots and
  makes the `Media` value (M2's `AddMedia` takes the whole entry, since
  `apply` does no I/O), and an agent's whole `media` object is refused.
  M2's limits refuse a second clip, so it matters from M3.

`set_view_path` is "follow the rider": one call sets orientation (and
field of view) keys at many times (M2 plan 4.1).

### 4.4 `undo`

`{file, redo?, if_revision?}`: one tool for both (4.5).

- **undo**: refused `nothing_to_undo` with no step below the cursor;
  `not_top` when that step is Pat's (its label, so the agent can say
  what Pat did); `stale_revision` when the history is stale (3.3) or
  `if_revision` isn't the project's. Otherwise undone, as a new
  revision, and saved.
- **`redo: true`**: the same, for the step above the cursor
  (`nothing_to_redo`, `not_top`, `stale_revision`).
- The result: the revision, the label undone or redone, and what undo
  or redo would do next.

### 4.5 The listing's budget

`tools/list` stays within PLAN's 12 KiB with every tool listed (writing
on). Each tool lists one title: the top-level `title` for clients on
2025-06-18 and later, `annotations.title` for older ones. Each tool has
a byte budget, tested in the slice that adds it, as `tools.rs` lists it
for the larger of the two forms:

| Tool | Budget (bytes) |
|---|---|
| A1's five, with `project` in look and check | 7,000 |
| `project` | 1,000 |
| `edit` | 950 |
| `undo` | 650 |
| `render` | 950 |
| `job` | 650 |
| `compare` | 1,000 |

That is 12,200 with the envelope. Descriptions say what the tool does
and when to use it, one time description is used word for word
everywhere, and the edit reference is a `project` part, not schema. If
the six can't be useful within these, the 12 KiB is Pat's to change.

The PLAN test: an agent's edit; the GUI opens the project, makes an
edit and saves; the agent's `undo` is `not_top` and the GUI's edit is in
the file (section 9).

## 5. Render jobs

### 5.1 `render`

`{project, output, range?: {from, to}, draft?}`

Within 1 s, and with nothing rendered yet, it:

1. loads the project (any root) and checks its media is beneath the
   roots, there, and the same file (`media_changed`, `media_missing`);
2. opens the output's folder beneath the write root and checks nothing
   is at the name and no queued render will write it (`output_exists`);
   the name is reserved in the job table until its job ends;
3. narrows the trim to `range` on a copy, in file time (4.1), refusing
   a range outside the trim (`out_of_range`);
4. checks the preset, the encoder and the frame plan, as
   `Export::start` does, with the same errors (D51's codes);
5. estimates the bytes needed, as the export's room check does
   (`export::needed_bytes`, moved out of `pipeline::run` so both share
   it), and refuses `no_space` with both numbers when the folder has
   less;
6. queues the job: `busy` when 4 are waiting already.

The result: `{job, frames, duration, size, draft, estimate_bytes,
ahead}`, where `ahead` is the jobs before it.

### 5.2 `job`

`{job, wait?, cancel?}`

- Gives the state (`queued`, `running`, `finishing`, `done`, `failed`,
  `cancelled`), frames done and in all, frames per second, the time
  left, the encoder and any fallback (D3), and once done, the report
  (the file, its bytes, codec, size, duration, audio, warnings) and the
  `check` of the output against the target its output fits:
  `youtube_360` for equirect, `flat_16_9` for 16:9, `vertical` for 9:16,
  and the rules without a target for 1:1 and 4:5 (A1's rules, D46). A
  failure gives the export's code and message.
- **`wait`** (seconds, at most 45) returns when the job ends or the
  time passes, or 2 s before the call's own deadline, sending MCP
  progress (frames done of all) while it waits if the call has a
  progress token. One `job` call waits at a time; another asks for the
  state at once and gets it, so the server always has a worker free of
  waits (`CALLS_AT_ONCE` is 2).
- **`cancel`** stops it (queued or running) and waits up to 2 s for the
  file to go; the result says `cancelled`.
- An id from another server is `job_lost` ("the server that ran it has
  ended; render again"); an unknown id from this one is `not_found`.
  The last 16 finished jobs are kept.

### 5.3 The job table and the output

`jobs.rs` holds the queue and one worker thread, started at the first
render. Ids are `j<8 hex>-<n>`: a random tag drawn when the server
starts, and a counter, so an id from an earlier server is known as one.

The worker runs `Export` as the CLI and the GUI do (M2 plan 8.6): its
own headless GPU (the adapter `spherecut mcp` was given), `Decoder::Auto`,
no pace. The export writes beneath the write root without a path that
could be swapped:

- **the output** is `/proc/self/fd/N/<name>`, N the output folder's
  descriptor from step 2, held by the job until it ends, with the
  export told to take the path exactly as given (`ExportJob::exact`):
  it follows no link there (the export's own `follow_links` calls, in
  `pipeline::folder` and `Mp4Writer::create_with`, are skipped), and
  its free-space check and sweep look at that folder. Its `TempFile` is
  made in that folder and published there with `persist_new` (D16),
  whose `link(2)` refuses anything that turned up at the name
  meanwhile, a link included. The report's path is mapped back to the
  agent's.
- **the media** is the project copy's media paths turned into
  `/proc/self/fd/M`, M each media file's descriptor from step 1, so the
  export decodes the file that was checked (D45).

The export's checks (M2 plan section 9) run before the file is
published, so the output appears only finished and validated; a full
disk ends it with `no_space` and no file (D52).

### 5.4 When the server ends

When its input ends, `rpc::serve` cancels the jobs at the moment it
stops the calls (`Tools::cancel_all`), then waits for both against the
same `EXIT_GRACE` deadline (`Tools::shutdown(until)`): the queue is
emptied, the running export cancelled, and its thread waited for, so
its temporary file is removed. A job that doesn't end by then is
counted in `Ended.stuck` and the process `_exit`s, as for a stuck call
(a1-plan amendment 20). What it leaves is a temporary file whose lock
went with the process, which the next export in that folder sweeps
(D16: its `flock` is free and it is past `SWEEP_AGE`, 10 s from A2.4,
since the age only guards the moment between a file's creation and its
lock).

## 6. The GUI

M2's GUI gains four things, none of them new windows:

1. **The lock** (3.2), taken on open, on the first save of an untitled
   project and on Save As (the target's), held until close and by a
   save still running then, with the refusal's notice; a folder it
   can't write opens without one, saying so.
2. **The history** read on open and written on every save (3.3), so
   Pat's Ctrl+Z goes through an agent's steps too. A stale or unreadable
   history opens as a fresh one, with a notice saying which.
3. **Changed on disk** (3.4): Save refuses and says so; Save As still
   works.
4. **Labels**: an agent's step reads "Undo Set trim (claude-code)" in
   the Edit menu and the history part.

Opening and saving already run as jobs off the UI thread (M2.6), and the
lock and history ride in the same jobs. The lock lives in `Doc` behind
an `Arc` that a save job shares, so closing the project lets it go once
any save under way has ended, and the process ending lets it go too.

## 7. Compare, and projects in look and check

### 7.1 `compare`

`{file, against_file? | against_project?, from?, times?, count?, sheet?}`
(flat, for the listing's budget: exactly one of the two `against_`)

- Decodes `file` and `against`'s file at each time (exact seeks, A1's
  `Still`), or renders `against`'s project at the time with the project
  at `file`'s size (as `frame --project` renders it, with motion blur
  when the sequence has it), and measures PSNR and SSIM with
  `render::compare`, the parity tests' code (M2 plan amendment 2 item
  32).
- **Times**: `times` (file time, at most 24), or `count` evenly spaced
  (default 8, at most 24) over the shorter of the two. `against`'s
  project is sampled at `from` plus the time, `from` being the trim's
  start by default, where a full render starts; a render of a `range`
  gives that range's `from`.
- **Sizes** must match for two files (`bad_argument` with both); a
  project is rendered at the file's size, refused when the aspect
  differs from the output's.
- The result: each time's PSNR (dB, `inf` for identical) and SSIM, the
  worst time of each, and the means. **`sheet`** adds an image: the
  worst four times, a row each of `file`, `against` and their difference
  times four, through A1's tiles, labels and inline budget.
- At its deadline it answers with the times it measured, `partial:
  true`, as `analyze` does.

### 7.2 `look` and `check` with a project

- **look** takes `project` in place of `file`: its frames are the
  sequence's output at the times (sequence frames, rendered as `frame
  --project` renders them, at look's sizes), as a sheet or one tile a
  time. `view`, `views` and the source modes are refused with a
  project; a view of the source is a look at its media file.
- **check** takes `project` in place of `file` and checks what a render
  would be: the output's kind and size, codec, audio choice and rate
  against the target's rules, each rule saying "a render would ...".
  A render's own check (5.2) is the proof; this is the check before.

Both schemas gain `project` and lose `file` from `required`; the op
refuses both or neither (`bad_argument`). Their two properties cost
about 200 bytes of A1's share of `tools/list`.

## 8. Teaching agents

### 8.1 Instructions

`INSTRUCTIONS` is rewritten to hold projects in the same 1,500
characters: the project tools are stateless and name the file; times in
a project are file times; an agent may undo only its own step on top;
render and then job with wait. The cheap-first order stays.

### 8.2 The skill

A fourth recipe, "Reframe a clip": probe and look to find the moments,
`project` create, `project` part `edits` once, `edit` with a view path,
`look` at the project, `render` with `draft` and a `range`, `job` with
wait, `compare` or `check`, then the full render. The skill test counts
four recipes.

### 8.3 `docs/agents.md`

The seven tools with their errors and limits (the docs test checks each
code and limit is listed), the edit reference, the files beside a
project, the undo rule, and **the GUI's actions that aren't edits**,
each with its tool or why it stays GUI-only: play, scrub and step
(`look` at the time); the viewer's drag (`edit` with `set_key` or
`set_view_path`); the overlay, Settings and the panes (GUI-only: they
change no project); Open, Save, Save As and Close (`project`, and every
write saves); Export (`render`); the curve editor's handles (`edit`
`set_key` with `ease`). Each milestone's review checks the list until
M5's shortcut map lets a test do it (PLAN).

## 9. Tests

### 9.1 Tier 1, core

- `ProjectFolder`: load and save through a descriptor whose path has
  since been renamed; a link at the name refused for writing and
  followed for reading; media paths relative to the real folder, not to
  `/proc`.
- **Two processes**: the test binary runs itself as a child (an
  environment variable says which part to play). The child takes the
  GUI's lock and waits; the parent's write is refused with the child's
  pid; the child exits; the write goes through. And: the parent loads,
  the child rewrites the file as an editor would, the parent's save is
  refused `changed_on_disk` and the child's text is what is there.
- The history file: written then read back; stale by revision; a
  history over 16 MiB trimmed oldest first; a redo's revisions moved
  (3.3); a crash between the two writes (the project write made to
  fail) reads as stale.
- Property test: any sequence of records, undos and redos, saved and
  loaded after each, gives the same project and history as one kept in
  memory, and undoing everything gives back the first project
  (`same_content`).

### 9.2 Tier 1, the tools (`crates/agent/tests/`)

With our own client over pipes, as A1's tests are:

- **Every `Edit` variant through MCP**: one `edit` call per op from
  `reference.rs` on a synthetic project, each applied and then undone
  through `undo`, the project after each undo equal to before. The ops
  M2's limits refuse (`add_clip`, `remove_clip`, a `remove_media` of
  the used media) are refused `bad_edit` with `limit` or `in_use`. A
  test fails when `Edit` gains a variant `reference.rs` lacks (the
  variants' op names, from serializing each, against the reference's).
- `if_revision` stale and current; `dry_run` saves nothing (the file's
  bytes and identity unchanged); all or none (a list whose last edit is
  refused leaves the file as it was); a refused edit's `at` is its
  index.
- The agent's undo: `not_top` under Pat's step (written by core as the
  GUI writes it), `stale_revision` after a hand edit, `nothing_to_undo`,
  and two agent undos in a row both allowed.
- Paths: a project outside the write root refused for writing and read
  from a read root; `edit` on a file that isn't a project refused
  `not_project` and left as it was; a project whose media is outside the
  roots gives `outside_roots` for that media from `render`, `look` and
  `compare`, and `project`'s summary says so without reading it.
- Times in edits: every A1 form, frames at the media's rate, a bare
  number refused (A1's rule), and the at/start/end pass test (4.1).
- `tools/list`: 11 tools and at most 12 KiB with a write root, 7
  without, each tool within its budget (4.5) in both title forms;
  annotations (`project`, `edit`, `undo`, `render` and `job` not
  read-only with writing on; `edit` and `undo` destructive, `render`
  not).
- The schemas and request types agree, as A1's do (every property in an
  example, required ones, unknown ones refused).

### 9.3 Tier 1, render and job (lavapipe)

- `render` answers within 1 s of the request, on a synthetic 2-second
  clip; frames done only ever increase over 100 polls; the job's report
  and its `check` pass.
- Cancel: within 2 s, and the output folder holds nothing new.
- **stdin closed mid-render**: `serve` returns within `EXIT_GRACE`, and
  the folder holds nothing new.
- **A killed server** (`crates/cli/tests`, `spherecut mcp` as a child,
  SIGKILL mid-render): its temporary file is there; it is aged past a
  minute (`utimensat`); the next server's render to that folder removes
  it.
- `job_lost` for an id from a previous server; `busy` for a fifth
  queued render; `no_space` from a folder on a small tmpfs, when the
  test can make one, else from `needed_bytes` against a faked free space.
- `range` and `draft`: frames and size as asked; a range outside the
  trim refused.

### 9.4 Tier 1, compare and the projects in look and check

- A file against itself: `inf` and 1.0 everywhere. A render against its
  project: above the export's parity floors (D52) at every time. The
  shifted control: a render against its project one frame off fails
  them.
- `sheet` within the inline budget; `partial` at a short deadline.
- look on a project equals `frame --project`'s pixels at the same time
  and size; check on a project of each preset passes its target, and a
  flat project checked for `youtube_360` fails the projection rule.

### 9.5 The GUI (`crates/app/tests/`, kittest)

- **PLAN's test**: an agent's `edit` (through `spherecut_agent::call`),
  the GUI opens the project (its Edit menu says "Undo Set trim
  (spherecut call)"),
  makes an edit and saves, closes; the agent's `undo` is `not_top`, and
  the file has the GUI's edit.
- The lock: while the GUI has the project open, the agent's `edit` is
  `project_locked` naming the GUI; after Close it goes through.
- A stale history opens fresh with its notice; a project changed on disk
  isn't saved over, and Save As still works.

### 9.6 Tier 2

`mcp_render_vaapi` in hw-test: an MCP session renders a few seconds of
one of Pat's samples on the hardware encoder, the job's check passes,
and `compare` against the project is above the hardware floors (D52).
odin runs it by Remote Control; huginn's waits for Pat, as before.

## 10. Limits

New, in `limits.rs` and listed in `docs/agents.md`, each with a test:

| Limit | Value |
|---|---|
| Edits in one call | 1,000 (a whole view path is one edit), within the 1 MiB request |
| A label | 200 characters |
| Keys a track in one `view` part | 200 |
| Steps in the `history` part | 50 |
| The history file | 16 MiB |
| Waiting for another call's lock | 2 s |
| Queued renders | 4, besides the one running |
| Finished jobs kept | 16 |
| `job`'s `wait` | 45 s |
| `compare`'s times | 24; 8 by default |

New error codes: `bad_edit`, `stale_revision`, `not_top`,
`nothing_to_undo`, `nothing_to_redo`, `project_locked`,
`changed_on_disk`, `not_project`, `bad_project`, `newer_schema`,
`media_missing`, `media_changed`, `no_space`, `job_lost`, and the
export's codes that `render` and `job` pass through (D51:
`cannot_export`, `no_encoder`, `encode_failed`, `mux_failed`,
`metadata_failed`, `check_failed`, `io_error`, and the hardware codes).
A project file's load errors map onto these, never D27's `too_large`.
`busy` also means a full render queue, and `cancelled` is a job's state
as well as the code an agent never sees. Each is in `codes::ALL` and the
docs.

## 11. Slices

Each ends with `cargo xtask check` passing and its tests; the last with
the full check, compat, proptests at 100,000, a fresh review against
PLAN's A2 criteria, and the note to Pat.

1. **A2.1 Projects on disk.** `ProjectFolder`, the lock, the history
   file, the identity check, `INITIAL_VIEW` and `project_media` moved;
   core's tests (9.1), the two-process ones included. D56's core half.
2. **A2.2 The project tools.** `CallCx::client`, the wire forms,
   `project`, `edit`, `undo` (with redo), `reference.rs`, the schemas and
   the list budget, `Roots::project_folder`; 9.2. D56.
3. **A2.3 The GUI.** Lock, history, changed on disk, labels; 9.5.
4. **A2.4 Render and job.** `needed_bytes` moved, `jobs.rs`,
   `Tools::shutdown`, `render`, `job`, the `/proc/self/fd` output and
   media, `call`'s refusal; 9.3, the killed-server test in cli. D57.
5. **A2.5 Compare, look and check with a project.** 9.4. D58.
6. **A2.6 Teaching and finishing.** Instructions, the skill's fourth
   recipe, `docs/agents.md` with the GUI actions list, the recipes
   replayed (A1's harness with a fourth), the Tier 2 test and its
   hw-test pickup, ARCHITECTURE, PROGRESS, PLAN's ticks, README; the
   review; the note to Pat.

## 12. Risks

- **`tools/list`'s 12 KiB** (4.5). The budgets leave little slack; the
  per-tool tests catch an overrun in the slice that causes it, and the
  limit itself is Pat's to change if the tools can't be useful within
  it.
- **The 1 s answer from `render`** reads the project (up to 64 MiB, D47)
  and hashes a MiB of the media. A large project on a slow disk could
  pass it; the test measures the synthetic case and the Tier 2 run a
  real one.
- **`/proc/self/fd` paths into FFmpeg and the export** extend A1's use
  (D45) to writing. A test renders through them on lavapipe before the
  rest of A2.4 is built on it.
- **Two windows of a race are stated, not closed**: between the identity
  check and the rename (3.4), and an older build that writes without the
  lock. Both need a writer outside Spherecut's own, and the first is
  microseconds wide.
- **The agent's name is what the client says it is.** It labels steps
  for Pat to read and decides nothing: an agent's undo is allowed for
  any agent's step, by the rule in 1.1, whatever the names.
