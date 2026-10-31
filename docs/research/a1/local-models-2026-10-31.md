# A1's tools with local models, 2026-10-31

`local_agents.py` (this folder) ran four tasks against four local models
served by llama-swap on huginn (Ryzen AI Max+ 395, 128 GB unified),
driven from odin at main a9d7028. The transcripts of every failed task
and each model's `summary.json` are in `hw-reports/agents-2026-10-31/`
(PR #15). Pat's email filter's model stayed loaded throughout; each
model was loaded only once the server showed that one alone.

The tasks, on `cargo xtask gen-media` clips with planted answers:

- **marker**: where the moving magenta marker is, as a yaw, which needs
  the model to see `look`'s images;
- **check-untagged** and **check-grid**: whether a 2:1 file passes
  `check youtube_360`, and which rules fail;
- **stutter**: `jitter-dups.mp4`'s gaps, duplicate timestamps and
  repeated pictures, from `analyze timing` and `motion`.

## Results

| Model | Sees images | marker | check-untagged | check-grid | stutter | Score |
|---|---|---|---|---|---|---:|
| 3.8-q4 | no | no answer: a 17k-token completion timed out after 9 calls | right | right | right | 3 of 4 |
| qwen3-coder-next | no | no answer: 14 views it couldn't see, the 16-step limit | no answer: right in prose, then `check` again every step, never the JSON | right | right | 2 of 4 |
| mistral-small-4 | yes | no answer: times made up past the 1 s clip, 15 of 16 calls refused | right | no answer: off-task calls with mutually exclusive arguments | wrong: gaps 0, though `analyze timing` said 2 | 1 of 4 |
| gemma-4 | yes | HTTP 500 on the first request, every task | | | | 0 of 4 |
| gemma-4, `--any-of-types` | yes | no answer: probe, a sheet, the sphere at three times, then views at yaw −90, then a completion past the 900 s timeout | right | right | right | 3 of 4 |

What they show:

- The deterministic tasks go through: `check` and `analyze` say enough
  for a model to report the answer, and 3.8-q4 and gemma-4 reported all
  three exactly. 3.8-q4 fixed an out-of-range time ("1" in a 1 s clip)
  to "29/30" from the error alone; mistral-small-4 sent refused
  arguments again and again.
- **marker** beat every model, through time rather than the tools: the
  two without vision can't do it, and the two with it ran out of steps
  or past the request timeout. gemma-4's approach was the skill's (a
  sheet, the sphere, then narrow views where the marker was), so a
  longer timeout is the next thing to try.
- **gemma-4's HTTP 500** is llama.cpp's Jinja engine failing on
  gemma-4's chat template ("filter-mapping not implemented") for a
  list-valued `type` nested under `items`: `look`'s `times` items are
  `["string", "array"]`. `analyze`'s `from` and `to` have the same type
  at a property and render. The same schema with `anyOf` of the two
  types renders, which `--any-of-types` sends. PLAN bans combinators
  only at a schema's root, so the fix is in Spherecut: time arguments
  now go as a nested `anyOf` of plain types, which `agent::schema`'s
  validator reads and its lint requires (D43).

Next: the same four tasks on the models huginn's upgrade brings
(nemotron among them), with a longer request timeout for marker.
