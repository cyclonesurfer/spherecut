# Progress

Updated 2026-10-30, with FFmpeg confined to the file Spherecut opened (D24), the CLI's JSON contract for probe and doctor (D27), odin's first hardware report, M1 slice 2a: `MediaBackend` with frame-exact seek in software, native pixel layouts and the colour defaults (D29, D30), slice 2b: hardware decoding through the backend, with fallback to software, the pooled download, the render node lookup and the Tier 2 seek tests (D30), slice 3a: the render core, checked on lavapipe against its CPU reference (D31), and slice 3b: the golden images with their negative controls and the render Tier 2 tests (D32), with the unsafe rule at every crate root (D28), and FFmpeg 8.1.3: the build break Pat found on odin, two faults Tier 1 found against 8.1.3 and one its review found, and `cargo xtask compat`, which lints and links against other FFmpeg builds (D2), and slice 4: `spherecut frame`, with the rest of the JSON contract (exit statuses 3 and 4, SIGINT, `--overwrite`), `TimeSpec` and `core::fs::TempFile` (D16, D27, D39), and the golden fixes for odin's third run: the encode pass's and the mip pass's own rounding to codes, and the other-driver rule's count of pixels past 4 LSB (D31, D32), and odin's fourth run (PR #6), where every Tier 2 test passed at 918d031 and the RX 7600's CPU checks came out as D32's emulation gave them, and slice 5's first part: the clocks, the UI thread's guard, and the frame cache with its budget, its second, the engine: the `Player`, its decode workers, the upload thread with its GPU slots, and the presenter (D34), and its third: the Tier 2 playback benchmarks and hw-test's H1 additions, the noisy perf clips, Tier 1 on the machine, its power settings and render nodes (D34, D37), and the fixes for the 19 faults the engine's review found (D34, D37) and for those its re-review and their tests found (D34), and slice 6: the file's audio decoded onto the clip's axis, the output crate on cpal with its audio clock, and video following that clock (D35), and slice 7: the GUI viewer on eframe and render's device, with the drag, zoom and projection switch, transport, the panes, the doctor's banners and the notices (D33, D36), and slice 5b's first path: VA-API frames imported into the GPU as DMA-BUFs, behind `SPHERECUT_ZERO_COPY=1` and `frame --zero-copy` (D25, D28). Milestone criteria are in `PLAN.md`;
decisions and their reasons in `DECISIONS.md`.

## Status

| Milestone | State |
|---|---|
| M0 Foundation | **Done** (2026-10-23). The huginn report at 4cd8a0f is clean: every Tier 2 test passes but one, which skips because RADV on Strix Halo has no Vulkan Video H.264 or HEVC decode. Evidence below |
| M1 Viewer | In progress. `VideoDecoder` (software, VA-API, Vulkan) is its first slice. FFmpeg's confinement (D24) is the second; its criterion is ticked since slice 4 gave `spherecut frame` its strace test. The CLI's JSON contract for `probe` and `doctor` (D27, `docs/cli-json.md`) is the third; its criterion is ticked since slice 4 brought `frame` into it, with exit statuses 3 and 4, SIGINT and `--overwrite` (evidence under "M1 criteria ticked", below). Slice 2a is the fourth: `core::video` and `core::cancel`, `media::backend` with `FfmpegBackend` and the frame-exact seek of plan section 3.3 in software (D29), native pixel layouts, swscale for the rest and the colour default rules (D30), synth's new clips and its `SyntheticBackend` and `GatedBackend`. The seek oracle (`crates/synth/tests/seek.rs`) is exact on every clip of the set, among them clips whose timestamps tie or go backwards and variable-rate Matroska files with a constant default duration; it checks what each seek cost and every run link, and its negative controls miss. The slice's review found 16 faults (wrong frames returned as `Exact` after a cut, in broken timestamps and in variable-rate Matroska, false run links when catching up, a duration missing for Matroska, an index trusted before Cues are read); all are fixed, each with a test that fails without its fix where one can show it (D29, D30). Its second review found 10 more (an open-GOP H.264 leading picture and a RADL picture returned after a seek, seeks past an MP4 cut, an MP4 spliced by its edit list read as constant-rate, resuming after a stopped seek in tied and backwards streams, an FFmpeg log capture that lost its lines to another thread, a test that a change in load could fail, and an overflow on a far-off Cues entry); all are fixed too, with tests where one can show it (D29). Slice 2b is the fifth: `DecodeMode::Hardware` decodes through the backend on VA-API or Vulkan Video, on one device per kind per backend opened on the render node `media::hw` finds (bus id, then PCI ids, then `renderD128`; `--render-node` overrides it), single-threaded, with frames kept on the device until handed out and downloaded into a pool of 256-byte-aligned buffers. `Fallback::Software` opens in software when the device can't be opened or turns the stream down, and reopens in software when it fails mid-stream, going on from the frame after the last one returned with no gap and no repeat; `Fallback::Fail` returns the error. A device's refusal or failure is its own error (`HwUnsupported`, the new `HwFailed`), never the file's damage. Media's `fault-injection` feature, turned on only by synth's tests, fails any frame as the device would, and the Tier 1 fallback tests (`crates/synth/tests/fallback.rs`) fail every frame of H.264 and HEVC clips while seeking, catching up and playing; they also fall back on lavapipe and on a render node that isn't there. The Tier 2 tests the plan names are written and listed (`vaapi_seek_matches_software`, `vaapi_colorinfo_matches_software`, `vaapi_download_pool_reuses_buffers`, `bench_seek_latency`, and Vulkan Video versions of the first three), and here they skip with their reasons; their checks pass on software standing in for a device and fail on one that fails. The slice's review found 4 faults (a stream cropped on the left came off the device narrower than software's frame, or failed on its first download at 10 bits, since FFmpeg's aligned cropping keeps columns by the pixel format's own rule; the pool tests couldn't tell a pool from a fresh buffer per frame; a device that won't open was named `vaapi`, and the Vulkan device's log line named the node asked rather than the GPU taken); all are fixed, with tests where one can show it. Both paths now crop exactly as the stream says, and the Tier 2 seek and colour sets have clips cropped on the left (D30). Slice 2 is done once 2b merges; its hardware results come with H1. Slice 3a is the sixth: `spherecut-render` on wgpu 30.0.1 (Vulkan only) with `rust-version` 1.95 (D1): the device (`Gpu`, `AdapterChoice`, `GpuCaps`, `RenderError` with contract codes), canonical plane textures (deep planes through an integer scratch and an ingest pass into f16) with area mips capped near 64 px, GPU slots, the `Uploader` (`write_texture` or a staging belt, one submit per frame), the projection shader (every projection and morphs, the shader's own LOD, colour from `ColorInfo`), the encode pass, `render_still` and the f64 CPU reference (D31), with sphere's `pixel`, `orient`, `uv_gradient` and `project_morph` and testkit's `compare` and `fnv64`. On lavapipe `render_still` matches the reference at 49.6 to 59.0 dB through every projection and ten plane layouts at 8 to 12 bits, both ranges, three matrices and three sitings, and the mips match an independent area mean; negative controls tell level 0 from trilinear and one siting from another (`crates/render/tests/`). The slice's first review found 11 faults; all are fixed, each with a test that fails without its fix where one can show it: lavapipe truncates an f32 stored into an f16 target, so half the 10-bit codes ingested a half low (the shaders now round every f16 store to the nearest half themselves); an upload's GPU time left out the ingest; a stereo frame's other eye bled in across the shown eye's edges (only the shown eye is uploaded now); the chroma siting had no check that didn't share the renderer's own arithmetic, and 4:1:1's factor was wrong at small widths; readback failed past wgpu's 256 MiB buffer limit; a lost device panicked in wgpu's error handler (every entry point now returns `gpu_failed`); and D31, D22, ARCHITECTURE and a test comment said things that weren't so (the GUI's device path, the crate count, FFmpeg in render's tests, the belt branch a test covered). An odd-width 360° frame's chroma step at the seam is recorded as a limit (D31). Slice 3b is the seventh: the golden images (`crates/render/tests/goldens.rs`, D32). One test runs every row of plan section 6 on lavapipe from synth's in-memory planes: six views stored as PNGs that must come back pixel for pixel at the Mesa build they were blessed on, every view against the f64 reference at floors calibrated when blessing, and minified views against synth's new `render_view_ideal`, the supersampled pattern itself. Each row's negative control is asserted on every run: implicit derivatives give 175 to 288 bad pixels at the seam and the poles where the analytic LOD gives none, level 0 shimmers 3.3 to 5.0 times as much as the ideal, and a swapped range, matrix or siting misses by 22 LSB, 19.5 dB and 53 LSB. A manifest records each row's driver build, source hash, view, thresholds and measurements; a changed source is reported as one and never compared as a render; and blessing (`SPHERECUT_UPDATE_GOLDENS=1`) writes nothing unless every row passes and every control fails. The evidence changed four of the plan's rules: the ideal comparisons cover only pixels 2.5 levels down or more, where level 0 must alias (over a whole view isotropic trilinear is softer than the ideal, and level 0 nearer it); range and siting are asserted at 10 bits, with 8 bits recorded, since lavapipe filters 8-bit planes to whole codes; `passthrough_exact` holds limited range to an independent model of the chain, and to round((k − 16) · 255 / 219) except within the f16 store's 0.0547 codes of a tie (86, 159 and 165 come out one code off); and each pole is turned to lie inside a pixel quad, which pitch ±90 exactly doesn't do. The Tier 2 tests `list_adapters_and_features`, `goldens_on_gpu_match_reference`, `goldens_on_host_llvmpipe` and `bench_upload_5k7` are written, each with a Tier 1 run on lavapipe; here the first two and the bench skip with their reasons, and `goldens_on_host_llvmpipe` passes, lavapipe being this machine's llvmpipe. Every crate root now forbids `unsafe`, except media's and the `spherecut` binary's, which deny it and allow it only in their FFI modules (D28). The slice's review found 10 faults; all are fixed, each with a test that fails without its fix where one can show it: a matrix or siting swapped inside render's colour code passed `matrix_601_709` and `siting_left_center` and was blessed, since the CPU reference shares that code (both rows now have truths outside it: the two matrices' codings of one picture must render alike, and smooth 4:2:0 chroma must match its 4:4:4 self); a blessing could lower any calibrated floor (it now prints them old → new and refuses a drop of more than 0.5 dB unless told `SPHERECUT_UPDATE_GOLDENS=lower`); the blessing command exited 101 after a blessing that had written everything (the harness's own tests read `tests/goldens` while it was rewritten, and now bless into scratch directories); three manifest entries lacked their sources or views; the controls' "over" bounds passed at the bound; `bench_upload_5k7` couldn't see a block made and freed within a step, which is its second trigger (it reads the allocator in flight too, and also runs two steps in flight), and timed 5.7K NV12 on the copy a decoded frame doesn't take (its frames now have the pooled download's stride); a GPU named by `SPHERECUT_ADAPTER` that refused a device showed as skipped; `round_f16` had no test of the subnormal halves and stored the largest half where the nearest is infinite; and a test comment was stale (D31, D32, m1-plan amendment 11). No golden image changed. Slice 3 is done. On 2026-10-24 main stopped compiling against FFmpeg 8 (3a8047d's swscale flags; fixed in 67fa1fd), which odin's `hw-test` found and nothing here had checked: Tier 1 now runs against an 8.1.3 built like Pat's, where it found that on one thread, as hardware decoders run, FFmpeg 7.1 and later's HEVC decoder gave a frame from before a seek after it (fixed, with a test, D29); odin's second run (PR #4) failed both GPU seek tests on it, and the download pool tests on a fault limit that counted the driver's faults (m1-plan amendment 12). Its review found that from 8.0 a damaged HEVC reference picture lost the rest of its GOP without a `Damaged` event, and an exact seek into it returned an earlier frame as `Exact`, so slice 2a's "damage the decode goes on past is an event" didn't hold on Pat's FFmpeg (fixed, with a test; on 7.1.x such damage is still silent, D29). Tier 1 against a 7.1.3 found that FFmpeg 7.0 to 8.1.2 leave an audio stream whose decoder they couldn't open without channels, which probe took for "has no channels" rather than the decoder's refusal, so a feature FFmpeg lacks became damage (fixed, with a test, D2, D15). `cargo xtask compat` lints the workspace and links its tests against other FFmpeg builds (D2). The review of that work found 10 more faults; all are fixed, each with a test that fails without its fix where one can show it: from 7.0 FFmpeg's H.264 decoder flags corrupt the frames decoded after a keyframe that isn't IDR, with no recovery point SEI, before it comes out, though their references are all there, and Spherecut took them for damage after a seek and dropped them (the oracle, which now fails any good clip that raises a `Damaged` event, has two such clips and a cut of one, D29); D13, D29 and this file said what FFmpeg's corrupt flag and its HEVC decoder do in ways that weren't so; probe made a PCM track with no channels damage even without samples (D15); a test gated what it expected on libavformat's version; `cargo xtask compat` deleted any directory without its stamp where a target went, the prefix itself when that directory was the prefixes' own, took a prefix whose `.pc` file led to another FFmpeg for found, and ran another pkg-config than the build does; and `build-ffmpeg.sh` said nothing of a step that failed (D2). Its second review found 7 more; 6 are fixed, each with a test that fails without its fix where one can show it, and one is a limit: such a stream with x264's default 3 reference frames never came out unflagged after a seek or at the start of a cut, so a cut played 2 of 32 frames and keyframe seeks went to the file's end (the I-frame decoding starts from is now taken as good, flagged or not, and the oracle has 3-reference clips and a cut, D29); a cut of the 1-B-frame clip cost a second seek after a keyframe seek (the keyframe a seek lands on now counts for the keyframes' lag); x264's intra refresh seeks only to where FFmpeg says decoding has recovered, which isn't always the decode from the start (a limit, D29); `build-ffmpeg.sh` left a half-installed prefix under its name; and compat checked libavcodec alone of the five libraries the build binds, let a target root with `..` build inside a prefix, and checked nothing once the build had run (D2). The review of that work found 5 more; all are fixed, each with a test that fails without its fix where one can show it: an exact seek ended on frames decoded from an I-frame that an MP4 lists as a sync sample but no IDR picture or recovery point vouches for, which may refer to frames before it (x264's I-frames at a flash, keeping 3 references: 28 of 40 exact seeks gave a wrong frame, with no event), so an exact seek in H.264 whose sync samples FFmpeg doesn't flag now decodes from the start of the stream (D29); a Matroska cut at the last keyframe of open-GOP H.264 opened on a leading B-frame decoded without its references (D29); and compat read the build's output where cargo doesn't put it once a build directory or target triple is set, held the libraries it checks to one line of the manifest, and its docs overstated its guard (D2). Its review found 7 more; all are fixed, each with a test that fails without its fix where one can show it: exact seeks still trusted FFmpeg's heuristic at an I-frame whose recovery point SEI counts frames, and 30 of 60 returned a wrong frame as `Exact` (now only a frame FFmpeg flags a keyframe starts an exact run after a seek, and x264's intra refresh, a limit until now, seeks exactly from the start of the stream); a seek back to the start of a cut at an I-frame that isn't IDR, with no B-frames, decoded other pictures than opening did, returned as `Exact` (such a start now decodes on a decoder opened afresh); the docs left the X5's LRV proxies to Pat's next run, where the samples here show them keyed by IDR pictures and seeking as they did (D29); and compat tied its libraries to the workspace's features only in a compat run, didn't count libavresample as FFmpeg's, failed every build in a target root with a brace, and said it recognised FFmpeg layouts it doesn't (D2). `cargo xtask compat` lints the workspace and links its tests against other FFmpeg builds (D2). Slice 4 is the eighth: `spherecut frame` (`crates/cli/src/frame.rs`) with every flag of plan section 5 and `--decoder vulkan`, on the JSON contract as `spherecut.frame/1` with exit statuses 3 and 4 (D27, `docs/cli-json.md`); `core::TimeSpec`, the exact `--time` grammar, with its proptest; `core::fs::TempFile`, moved from `inject`, with `persist_new` (a hard link, which fails with EEXIST whatever turned up at the output, or an exclusive create and a copy where links are refused) beside D16's `persist_replace`; and `cli::interrupt`, the binary's one FFI module, whose SIGINT handler (`SA_RESETHAND | SA_RESTART`) only stores to an atomic that the main thread polls every 10 ms while a worker thread does the work (D28). `--decoder auto` decodes with VA-API on the render node of the GPU that renders and falls back to software, saying why in the result (D39). On lavapipe the Euler table's centre pixels are F (255, 1, 0), R (0, 255, 1), B (1, 0, 255) and L (254, 255, 0), and at the poles the neutral greys 184 (U) and 72 (D) that the f64 reference gives too, since isotropic filtering at the pole blends the marker disc with the grid round it (D32); `--roll 90` keeps F at the centre and puts U on the view's left (amendment 10), shown in a 200° fisheye. The frame code read back through a 360° fisheye is the frame `--time` and `--frame` ask for on a constant-rate MP4 and on its Matroska remux at 1/1000, and `--frame` counts frames on a variable-rate clip. Every error row of the exit table leaves no output and no temporary file; an existing output, a link or a dangling link is never replaced without `--overwrite`, and a file created between the check and the publish is kept (`output_exists`). Ctrl+C 300 ms into a 600-frame exact seek on `long-gop` ends the run about 25 ms later with exit 4 and `cancelled`. The pipeline goldens (D32) store four PNGs, identical here at the build they were blessed on, that `frame --reference` matches at 51.2 to 90.0 dB, and the swscale cross-check is within 1 LSB of swscale and 0.51 to 0.60 LSB of the exact conversion at BT.601 and BT.709, both ranges and both depths, where a swapped matrix or range misses by 18 to 30 LSB. strace shows no IP socket for `frame`. The Tier 2 `vaapi_render_matches_software` and `vulkan_render_matches_software` are written and listed by hw-test, and skip here with their reasons; the same check passes on lavapipe with software standing in for the device and fails on the next frame. Two M1 criteria are ticked: FFmpeg's confinement and the JSON contract. The slice's review found 18 faults; all are fixed or corrected in the docs, each with a test that fails without its fix where one can show it: a frame past a Matroska cut was rendered as the frame asked for (the decoder now finds a Matroska cut from the Segment's size, D13, and `--frame N` on a constant-rate stream checks that the frame shown is N); `--frame N` past a cut on a variable-rate clip failed `decode_failed` rather than by its cause; a Matroska equirect was refused as having no spherical metadata (frame now says it doesn't read Matroska's yet); `--time` and `--frame` values past what the time base holds failed `decode_failed`; negative values got clap's tip; `-o dir/` was refused only after the render; the SIGINT handler replaced an inherited `SIG_IGN` and was installed after the temporary file was made; the Vulkan Tier 2 test skipped on a broken device, passed with clips left out and lacked FFmpeg's words, and no Tier 2 run tried `--decoder auto`; and the tests couldn't tell `--reference` rendering on a GPU, or a crop's sides swapped. The docs' Ctrl+C, output, timing and field lists are corrected, and that lavapipe's `aniso` renders as `iso` is recorded (D13, D16, D27, D39). No golden image changed. odin's third run (PR #5) failed two golden tests, for three reasons, and branch `m1/goldens-hw` fixes all three: the encode pass rounds to its target's codes itself, since RADV's export packs to f16 toward zero first and put ten of `passthrough_exact`'s codes a code low (D31); the other-driver rule allows at most 4 pixels more than 4 LSB off and none more than 8, which passes LLVM 22's maximum of 5 LSB if no more than 4 of its pixels are past 4 LSB (odin's next run notes the count) and fails every fault made from the stored views: a pixel's shift, a seam, pole or rim blacked out, and implicit LOD wherever it shows (D32, m1-plan amendment 13); and the mip pass rounds its 8-bit means to codes itself (below). The branch's review found 13 more; all are fixed or corrected in the docs, each with a test that fails without its fix where one can show it: the fault test read `tests/goldens` while a blessing rewrote it, which failed the blessing command after it had written everything; the encode pass's 10-bit output, packed to f16 toward zero, could land 0.0005 codes from a tie, and its output into an f16 target was the driver's rounding (it returns halves there now, D31); a Tier 1 assertion held whatever lavapipe the machine has to a model of one lavapipe build; the other-driver path's test couldn't tell the two rules apart, and the CLI's rule had no test; and D32, m1-plan, ARCHITECTURE, perf.md and this file said things that weren't so (a fix where a floor was lowered, LLVM 22's unrecorded count, a fault table's SSIM, the labels' darkest pixels, a filter's cost, an RMS). No golden image changed there either. Then the mip pass: it works each 8-bit mean of codes out exactly, in integers, and rounds it to the nearest code with ties to even, as the CPU reference now does, after an emulation showed that its stores, left to the driver, not RADV's filtering, cost the RX 7600's wide views up to 15 dB (D31, D32). A lower floor for a GPU's CPU checks, the RX 7600's measurement less 3 dB, stood in for that fix for a while and is gone; the emulation predicts the RX 7600 at 56.11 to 59.78 dB after it, 7.91 dB or more above lavapipe's floors. The fix moved six stored goldens and one pipeline golden by up to 4 LSB where a pixel reads mip level 1 or more, and raised all nine CPU floors of the render goldens and that pipeline golden's (47.2 to 48.17 dB); the other three pipeline goldens' didn't move. odin's fourth run (PR #6, at 918d031) passed all 25 Tier 2 tests: the RX 7600's CPU checks came out at 58.88 to 60.75 dB, 8.57 dB or more above the floors and, at every check, the emulation's own number to the hundredth of a dB, so the error its prediction had added as RADV's own wasn't RADV's; `passthrough_exact` gave limited 86, 159 and 165 one code off and nothing off the model, as lavapipe does; and LLVM 22's llvmpipe put no pixel more than 4 LSB off a stored image (D31, D32). Slice 5, the headless playback engine, is the ninth, in three parts. Its first part is the clocks, the UI thread's guard and the frame cache (D34). `core::clock` has the `Clock` trait, `SystemClock`, and `ManualClock`, which tests and M2's export step frame by frame. `core::blocking` has `mark_ui_thread` and `may_block`, and every blocking helper in core, media, render and synth now calls `may_block`, which panics on a marked thread in debug and test builds. The new `spherecut-playback` crate has the bounded `FrameCache`: frames linked by run, a window ahead of the playhead that is never evicted, eviction of the frame farthest behind first, and back-pressure that stops with its token. It also has the cache's budget, from `/proc/meminfo` and cgroup v2 limits or a size from `SPHERECUT_FRAME_CACHE_MIB`, and `PerfStats`. A proptest holds the cache to a model of its rules at 100000 cases, and caught each of eleven faults put into the cache by hand. The second part, the engine, is in too (D34). The `Player` takes commands that return at once and is read with `try_lock` and atomics only. A decode worker per video stream answers seeks from the cache's runs or the decoder, the latest request winning, decodes ahead under the cache's back-pressure, and catches up when more than two frames behind the clock, counting what it passes as decode-dropped. Its decoder is picked from doctor's checks (Vulkan Video on a discrete GPU when both kinds work), and a fallback reaches the snapshot. The upload thread fills the GPU slots, reusing one only when it is Free and nothing else holds its frame. The presenter shows each stream's latest frame at or before the clock, which suits variable frame rates, and keeps the frame up until a seek's is ready. Shutdown joins every thread within one budget, and there is a seam for slice 6's audio. On a marked UI thread, with a backend that never answers and through a storm of 1000 seeks in 2 s, UI iterations took under 200 µs at the 99th percentile; the storm decoded one seek to completion. A 5.7K source played every frame inside a 256 MiB cache, peaking at 237 MiB. The third part, the last, is in too (D34, D37). The Tier 2 playback benchmarks play hw-test's perf clips, the X5 raw files and any stitched sample through a `Player` on the machine's GPU, from a thread marked as the UI thread at 60 Hz deadlines, projecting each new frame to 1920×1080, once unthrottled and once in real time. `bench_playback_realtime_5k7_8bit` and `_10bit` hold the hardware decoder `Auto` picks to plan section 12.3's rules: no drop after the first second of 60 s, and every unthrottled second at 1.2 × 29.97 frames or more. Their harness runs in Tier 1 on lavapipe. hw-test makes the perf clips once, 5.7K and 8K HEVC with noise on every frame at a camera's bitrate, runs Tier 1 on the machine, and reports its power settings and render nodes. The slice's review found 19 faults; all are fixed or corrected in the docs, each with a test that fails without its fix where one can show it: two video streams, as the X5's lenses, deadlocked on GPU slots after a seek or step on three slots, with a slot dead or with the second decoder late; a keyframe seek back with the cache full never landed (the landing is now pinned while its request is the target); a step made while a seek or step was on its way started from the frame on screen, and two instant steps before a present went one frame; a paused frame moved with the A/V offset; a 5 s budget recheck counted the cache against itself, and the cgroup cap missed a namespaced container's limit; two Tier 1 tests would have failed on odin's Vulkan Video, and xtask's machine tests raced; and tests, a count and docs said or checked less than they seemed to ("Slice 5", below, D34, D37). Its re-review found 5 more, and their tests 2: a keyframe seek back on two streams on few slots could stay in Seeking, a failing stream's slots had no test, a step during a keyframe seek started from the time asked, a step superseding a seek before its worker started it could wait for room for good, one lens's next frame could take the slot the other's needed on three slots, the budget's recheck swung under pressure, and a cgroup's page cache counted as use; all are fixed, each with a test that fails without its fix, and two slots Dead of four on two streams is a documented limit (D34). None of it has run on a GPU yet: H1 is requested (Next, item 1), and the real-time criterion waits for it ("Slice 5", below). Slice 6, audio, is the tenth (D35). `media::audio` decodes a file's audio through its own confined demuxer and decoder to packed f32 at the output's rate and channels, on the clip's time axis: AAC's priming gone, edit lists applied, mono duplicated, first-order ambiX decoded to two cardioids, the rest mixed down, and seeks that give a sequential decode's samples within 1e-6, 44.1 kHz resampled included. The new `spherecut-audio` crate plays it through cpal (PulseAudio's protocol, else ALSA), from a lock-free chunk ring, with an audio clock that video follows, so the sound card's clock is the master; it falls back to the system clock when the device is lost or stalls and comes back to the audio without a jump, and plays on a null output where there is no device, as here. The engine opens a file's audio with the file and prerolls for it after a seek. The A/V tests run the engine on a scripted device at 29.97 and 59.94 fps with 44.1 and 48 kHz for 60 s each, and pass; the Tier 2 device test is written and skips here, for H2 ("Slice 6", below). H1 has run on odin (PR #7, at 43abcda): its two 5.7K gates and four Tier 1 tests failed, and the X5 raw benchmark passed on passes that had stalled. Branch `m1/h1-fixes` fixes all of them: the benchmarks let go of their first frame, so two streams play; after a seek back the frames left ahead keep out of the GPU slots and the clock waits for the next two frames; and the download pool's buffers are on huge pages, so a 10-bit 8 GiB cache fills in about 2.5 s rather than 7. At its commits odin dropped no frame after the first second on either gate in any run, and the 10-bit gate's unthrottled clause failed once in four runs, at 17 to 20 fps for 3 s while the cache filled ("odin's fifth run", below). huginn's H1 is still to come. Slice 7 is the GUI viewer: `spherecut [file]` opens it on render's own device, and the kittest suite proves it on lavapipe, byte for byte against `render_still` at four scales, the drag within 0.34 px, and the whole app never waiting on a backend that never answers ("Slice 7", below). The criterion that the UI never blocks is ticked with it. odin's sixth run (PR #8, at 303e364) passed every Tier 2 test it could run, both gates on Vulkan Video with no frame dropped, and failed one Tier 1 test that needed more time under load, now fixed ("odin's sixth run", below). Slice 5b's path A, zero copy from VA-API, is built, and on odin (PR #9) every frame came through byte for byte and VA-API's X5 modes decoded twice to three times as fast imported ("Slice 5b", below); odin's run also showed that VA-API's 8K slowdown was the download pool's mappings, which the pool now leaves to Vulkan Video |
| M2–M9 | Not started |
| Agent integration (track A) | Planned 2026-10-24 at Pat's request (`PLAN.md` track A, DECISIONS D23). The CLI's JSON contract and FFmpeg's confinement are M1 criteria; the confinement is in (D24), and so is the contract for probe and doctor (D27); A1, the MCP server, follows M1's code |

## Environments

### Cloud workspace (where the code is written)

| | |
|---|---|
| OS | Ubuntu 24.04.5 LTS, 4 CPUs, 15 GB RAM |
| Rust | 1.97.0 stable (rustup) with rustfmt, clippy, cargo-deny 0.20.2, cargo-audit 0.22.2; 1.95 for the MSRV check (1.88.0 before slice 3a). clang 18.1.3, strace 6.8 |
| FFmpeg | 6.1.1 (Ubuntu), with -dev headers, libx264, libx265, libdav1d, SVT-AV1. Also built from upstream tags for the compatibility checks in D2: minimal 7.1, 8.1 and 9.0.2; 9.0.2 with VA-API, libx264, libx265 and libdav1d; and 8.1.3 (libavcodec 62.28.103, as on odin and huginn) and 7.1.3 (libavcodec 61.19.101; Fedora 43 has 7.1) with VA-API, Vulkan, libdrm, libx264, libx265, libdav1d, SVT-AV1, Opus, Vorbis and libshaderc, as `scripts/build-ffmpeg.sh` builds them, on Khronos Vulkan-Headers 1.4.354 over the system loader (Ubuntu's 1.3.275 headers are older than 8.1's Vulkan needs). The libdrm, Opus, Vorbis, shaderc and SVT-AV1 -dev packages it needs were installed on 2026-10-24; the 6.1 bindings and link lines built after are byte for byte those built before. No `glslc`, so no SPIR-V compiler at build time |
| Vulkan | lavapipe only: llvmpipe, Mesa 25.2.8, Vulkan 1.4.318 |
| Devices | no `/dev/dri`, no GPU, so no VA-API and no Vulkan Video; no sound card (ALSA's `default` PCM isn't there) and no sound server, so audio plays on the null output; ALSA's headers (libasound2-dev 1.2.11) for the build |
| Network | crates.io, GitHub and Ubuntu apt reachable through a proxy; ffmpeg.org blocked (recorded at setup) |

Here Tier 1 runs fully. `SPHERECUT_TIER2=1 cargo test --workspace
--no-fail-fast -- --ignored` runs the 32 Tier 2 tests.
`goldens_on_host_llvmpipe` passes, since lavapipe is this machine's
llvmpipe, and the others skip: those that need VA-API with "no DRM
render node at /dev/dri/renderD128", the Vulkan Video ones with "no GPU
for Vulkan Video: FFmpeg's Vulkan device is llvmpipe …", the render ones
that need a GPU with "no GPU: Vulkan lists only llvmpipe …",
`vulkan_render_matches_software` with "the only adapter is a CPU:
"llvmpipe (LLVM 20.1.2, 256 bits)"", `audio_output_device_plays` with
"no audio output device: ALSA: The requested audio device is not
available. It may have been disconnected.", and,
without `SPHERECUT_SAMPLES`, the samples tests with "samples directory
samples not found". A skip counts as a failed test to cargo, so
the command exits non-zero, and without `--no-fail-fast` cargo stops after
the first test binary. `cargo xtask hw-test --quick` runs them the same
way and reports each one. With `SPHERECUT_SAMPLES` pointing at the
extracted X5 samples, `samples_probe_cleanly` passes.

### Pat's machines

| Machine | Hardware | Software | State |
|---|---|---|---|
| huginn | Ryzen AI Max+ 395, Radeon 8060S (Strix Halo, RDNA 3.5), 128 GB | Fedora 44, kernel 7.0.9, Mesa 26.2.5 (RADV, radeonsi from `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 (Fedora RPM) | `hw-test` at 4cd8a0f, 2026-10-23: 8 passed, 1 skipped. VA-API decodes H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software, and encodes H.264, HEVC and AV1. Vulkan Video decodes AV1 but not H.264 or HEVC ("Device does not support decoding hevc!"). Earlier run at a9128cf: PR #1 |
| odin | RX 7600 (Navi 33, RDNA 3, discrete; the only GPU) | Fedora 44, kernel 7.2.9, Mesa 26.2.5 (RADV and radeonsi from RPM Fusion's `mesa-vulkan-drivers-freeworld` and `mesa-va-drivers-freeworld`), FFmpeg 8.1.3 (RPM Fusion), rustc 1.99.0 | `hw-test` at be38fa4, 2026-10-24 (PR #5): 21 passed, 2 failed, both goldens (above). At 67fa1fd (PR #4): 15 passed, 4 failed, all four this side's and fixed after it (the HEVC flush in both GPU seek tests, D29; the download pool's fault limit and fault-counter check, amendment 12). Exact seeks on the GPU take about 0.2 s on 8K30 X5 footage and under 55 ms at p95 on the LRV. Earlier run at 44df411 (PR #3): 12 passed. VA-API and Vulkan Video decode H.264, HEVC Main and Main 10 and every X5 sample bit for bit like software. Only Vulkan Video decodes every X5 mode faster than real time (`docs/perf.md`) |
| Framework 13 Pro | Intel Xe3 | Arch (Omarchy), Hyprland | Arrives late 2026 |

Access: none direct from the cloud workspace. Remote Control on odin is
set up for another project's folder, and a CLI session on huginn wasn't
reachable from here, so hardware runs go through a prompt Pat pastes into
a Claude Code session on the machine, which opens a pull request with the
report (PR #1 for huginn). That session works from a clone of the GitHub
repository, so a commit has to be pushed before it can be run there.

## Samples

- **Insta360 X5**, firmware v1.1.22 (`probe` can't confirm it until the
  trailer is parsed), 7 takes with LRVs, clips 1–4 and 8–10 of
  `docs/SAMPLES.md`: 8K30, 8K25, 5.7K60 (HDR video mode), 4K120, the
  tripod seam clip, the handheld flips and the slow rotation. All 14 video
  files probe cleanly at d2975a6, with no damage, warnings or FFmpeg log
  lines, and are detected as Insta360 dual fisheye or LRV proxy. Findings:
  `docs/formats/insta360.md`, "Pat's X5 samples". Still wanted: I-Log, 360
  audio, FlowState on, a Studio export, a split recording.
- GoPro and DJI: none yet.

## M0 acceptance criteria

| Criterion (`PLAN.md`) | Evidence |
|---|---|
| Environment recorded | The tables and the access note above |
| Opening questions and sample request | Sent 2026-10-22; the sample list is `docs/SAMPLES.md`. Pat answered with Insta360 X5 samples; the rest of the list is still wanted (Samples, above) |
| Workspace | Crates `core`, `sphere`, `media`, `synth`, `testkit`, `cli`, plus `xtask`; all GPL-3.0-or-later. Workspace lints deny `unsafe_op_in_unsafe_fn` and undocumented `unsafe` blocks; `unwrap_used` and `expect_used` warn, allowed only in tests (`clippy.toml`), and clippy runs with `-D warnings` |
| `cargo xtask check` | `xtask/src/check.rs`. `cargo xtask check` passes at 2c8d0c0: rustfmt, clippy, the Tier 1 tests, doc tests, `cargo deny` (advisories, bans, licenses, sources) and `cargo audit` |
| `cargo xtask gen-media` | 12 clips (grid, high-frequency and seam patterns; H.264 and HEVC; 8 and 10-bit; 25, 29.97, 30 and 59.94 fps; stereo and ambiX; with and without Spherical V2; a 4K clip; lossless variants) and 12 broken files: empty, not media, cut at three points, and one for each kind of `moov` damage FFmpeg opens without complaint (chunk offsets, a `trak` type that is not text and one that still is, an `mdhd` timescale, a video codec configuration, an AAC setup and a `proj` type). `rendering_is_deterministic` in `crates/synth/src/lib.rs`; two full runs at c2b2753 gave byte-identical output over all 25 files, the clips, the broken files and `MANIFEST.md` (D9) |
| `spherecut probe` | `crates/cli/tests/probe.rs`, among them `file_names_that_are_not_utf8_are_reported`, `files_named_in_bytes_that_are_not_utf8_are_probed`, `a_codec_ffmpeg_does_not_know_is_a_warning`, `an_audio_setup_ffmpeg_does_not_implement_is_a_warning`, `an_empty_track_ffmpeg_cannot_set_up_is_a_warning`, `ffmpegs_log_is_kept_for_a_file_it_cannot_open` and `a_reader_that_goes_away_is_not_an_error`; the `probe` unit tests on which streams FFmpeg cannot decode are damage (`a_stream_ffmpeg_cannot_set_up_is_damage_only_with_samples_it_reads`, `a_stream_without_a_decoder_in_this_build_is_a_warning`, `a_decoder_that_refuses_the_codec_setup_is_damage_unless_it_lacks_a_feature`); `tags_that_are_not_utf8_come_out_as_valid_json` in `crates/cli/tests/probe_tags.rs`; `a_file_whose_name_is_not_utf8_probes_and_decodes` in `crates/media/tests/non_utf8_names.rs`; `broken_files_give_errors_not_panics` in `crates/synth/tests/generated_media.rs` (every bad file, plus 4000 edits of a real `moov`, of which it must find at least 3000; it found 3453 at 1bfa90b); the `mp4` unit tests `timescales_ffmpeg_cannot_use_are_damage`, `box_types_that_are_not_text_are_damage_where_types_are_registered`, `a_trak_under_another_printable_type_is_damage`, `an_sv3d_ffmpeg_ignores_is_damage` and `zero_types_are_allowed_only_at_the_end_of_a_sample_entry`, and the MP4 fuzzing (D15) |
| `spherecut doctor` | `crates/media/tests/doctor.rs`, `crates/cli/tests/doctor.rs` against lavapipe, and the unit tests in `crates/media/src/doctor.rs`, among them `every_missing_codec_gets_an_issue_with_the_ffmpeg_fix` and `a_driver_that_decodes_wrongly_is_told_apart_from_one_that_refuses`. `crates/media/tests/hw_refusal.rs` checks, on lavapipe, the refusals that doctor's probes share with `VideoDecoder` (D14). Capability checks and fixes: D18 |
| `cargo xtask hw-test` | Unit tests in `xtask/src/hw_test.rs`, among them `paths_that_are_not_utf8_reach_the_tests_unchanged_and_are_reported` and `benchmarks_show_median_range_and_spread`, and `samples_with_a_path_that_is_not_utf8_are_used` in `crates/testkit/src/lib.rs`; the huginn run; the reporting rules in D19 |
| First hardware report | `hw-reports/huginn-a9128cf8df-dirty.md`, then the clean `hw-reports/huginn-4cd8a0f744.md`: toolchain, a Vulkan device (RADV), VA-API HEVC decode and encode working, doctor's decode checks naming exactly what Vulkan Video can't decode, and the benchmarks with their spread (`docs/perf.md`) |
| Docs | README, PLAN, ARCHITECTURE, DECISIONS, PROGRESS, `docs/PROMPT.md`, `docs/SAMPLES.md`, `docs/formats/{insta360,gopro,dji}.md`, `docs/perf.md` |

The M0 review at a9128cf found two blockers (B1, B2) and six should-fixes
(S1 to S6). They are addressed by the merges fa7d370 (hw-test), 8c664d2
(FFI and decoder), c12a4d4 (doctor), 1a3f9c7 (writers) and 4603a56 (MP4
and probe), and by 9300fc0 and e33b77b. The original S1 file (the
`bad-corrupt-moov.mp4` that a9128cf generated) fails `probe`. So does the
file that a9128cf's generator, with the same seed, makes from today's
first clip, which C6 below found still passing.

The final review at d47d7e7 found C1 to C19. They are addressed by the
merges 0cf475e (decoder: C1's library half, C2, C12, C13, C16, C18 and
C19), 15c68b2 (probe: C1's CLI half and C6) and 5b3d02d (doctor, inject
and hw-test: C4, C7 to C10, C14, C15 and C17), and by 721393a (C5 and
C11, the Tier 2 command above). d2975a6 stops `spherecut probe *.mp4 |
head` from panicking when `head` exits. C3, pushing the M0 work, was
done with 2c8d0c0.

Fresh verifiers of those fixes found smaller problems, fixed in the
merges a88d331 (decoded planes stay inside their own plane, the AV1
decoder choice is tested, doctor no longer says wrong output falls back
to the CPU, and Fedora's no-GPU fix drops a useless install line) and
5503e8e (probe opens each stream's decoder, finds renamed and zeroed
boxes, keeps FFmpeg's log lines whole and keeps them when probe fails,
and the samples tests use a directory whose path is not UTF-8). c2b2753
counts a box inside `moov` whose size is 0 as damage. Each branch was
checked by a fresh verifier before it was merged. A last review of the
commits made on main found a closed stderr still panicking (0b479e6,
adead9f), a zeroed `elst` going unnoticed (e93d693, 8462720) and `inject`
writing after a QuickTime terminator (324e224), all fixed with tests.

huginn's rerun at 4cd8a0f (PR #2, `hw-reports/huginn-4cd8a0f744.md`) came
back clean, so M0 is done.

## Resumed 2026-10-28

- `m1/frame` (slice 4, `spherecut frame`) is finished, reviewed and
  merged.
- `m1/compat-fixes` is merged: the second review of the FFmpeg 8.1.3
  work found that good H.264 without recovery-point SEI raised `Damaged`
  after seeks on FFmpeg 7.1 and later (its corrupt flag comes from
  recovery state, not a lost reference), and that SEI-less H.264 keeping
  3 references could seek to the wrong keyframe; that a zero-channel PCM
  stream became damage instead of a warning; and that `cargo xtask
  compat` could delete a directory it hadn't made and report a pass
  against the system FFmpeg. All are fixed with tests, and a third
  review checked the fixes (D2, D13, D15, D29). An x264 intra-refresh
  stream still sought only to the frame FFmpeg calls recovered, until
  `m1/h264-landing` (below).
- `m1/h264-landing` is merged: the review of that fix found exact seeks that
  returned wrong frames as `Exact` in H.264 whose `stss` lists I-frames
  with no recovery point that later frames refer across, a Matroska cut
  that opened on a leading B-frame, and three faults in compat's build
  checks. All are fixed with tests (D2, D29). Exact seeks in such H.264
  now decode from the start of the stream; the X5's LRV proxies aren't
  such streams (their keyframes are IDR pictures), and their exact seeks
  cost what they did. Its review found that exact seeks still trusted
  FFmpeg's heuristic at I-frames whose recovery point counts frames, and
  that a seek back to the start of a cut at an I-frame that isn't IDR
  decoded other pictures than opening did, both returned as `Exact`, and
  five nits in compat and the docs; all are fixed, with tests where one
  can show it (D2, D29). x264's intra refresh now seeks exactly too,
  from the start of the stream; so does a stream keeping one reference
  whose I-frames that aren't IDR are listed (a remux), which was right
  and now decodes about 2.8 times the frames (D29).

## M1 criteria ticked

The first two were ticked in `PLAN.md` with slice 4, whose acceptance
(m1-plan section 14) meets them in full. Neither depends on a GPU or a
hardware decoder: both are about what FFmpeg may open and what the CLI
prints. The third was ticked with slice 7.

| Criterion (`PLAN.md` M1) | Evidence |
|---|---|
| FFmpeg opens only the file Spherecut opened | Slice 1 (D24): `ffi::confine` (our own AVIO, the `io_open` refusal, the demuxer allow-list, `protocol_whitelist`, `format_whitelist`, `max_pixels`) under probe, the decoders and doctor; the root `clippy.toml`'s `std::net` ban; `crates/cli/tests/confinement.rs` (the HLS playlist naming a file outside its folder, ffconcat, an `http://` segment and the path `http://127.0.0.1:1/x.mp4` refused with the reason, files named like protocols read as files); probe refusing other containers by name (the `lrc` test in `probe.rs`, the README, D15); strace a prerequisite of `cargo xtask check`. Slice 4 adds `frame_opens_no_network_socket`: `spherecut frame clip.mp4 --adapter cpu -o out.png` under `strace -f` opens no `AF_INET` or `AF_INET6` socket and connects only to Unix sockets, next to `probe_opens_no_network_socket_and_no_listed_file` |
| Decode, audio and texture upload on workers; the UI never blocks on them (a harness injects a slow decoder); a bounded frame cache sized from memory, configurable | Ticked with slice 7 (m1-plan section 14: slices 5, 6 and 7). The engine's `never_blocks` (`crates/playback/tests/never_blocks.rs`: a fake UI loop on a thread marked with `OnBlock::Panic` drives a player whose backend never answers, and one whose decoder takes 50 ms a frame through 1000 seeks in 2 s, each iteration's work under 2 ms at the 99th percentile; `may_block`'s negative control); the GUI's (`crates/app/tests/never_blocks.rs`: the whole app on a gated backend, the harness thread marked and the guard shown live, 60 steps each under 250 ms, 20 ms at most here, and the first frame two steps after the release); `crates/app/clippy.toml`'s bans with every `#[expect]` fulfilled; the audio's threads (D35); the cache's proptests and `auto_budget`'s fixtures (D34), and `--cache-mb`, `SPHERECUT_FRAME_CACHE_MIB` and the budget's default of a quarter of what's available, 512 MiB to 8 GiB |
| The CLI's JSON contract (probe, doctor, frame) | `crates/cli/tests/contract.rs`: one compact document per run on success and failure, a panic and a usage error included (`a_panic_is_one_internal_document_and_exit_1`, `usage_errors_under_json_are_a_document_and_exit_2`, `stdout_carries_only_the_document_whatever_the_log_says`); schema and version in `schema`, `ok`, stable codes listed literally and documented (`every_code_is_listed_and_stable`, `the_document_describes_every_code_schema_and_status`); one shape for one file or many; times as `{exact, seconds}` plus `frame` (`times_and_rationals_read_back_exactly`, and frame's result in `frame_documents_have_the_envelope_too`); exit statuses 0, 1 and 2 for probe and doctor (doctor's errors exit 1), and 0 to 4 for frame: `frame_exits_0_to_3_with_one_document_each` (no Vulkan driver through `VK_DRIVER_FILES`, `--decoder vaapi --render-node /nonexistent`) and `sigint_during_a_seek_cancels_with_exit_4`; `only_frame_handles_sigint` (probe and doctor keep the default action) and `an_ignored_sigint_stays_ignored` (an inherited `SIG_IGN` is kept). `--overwrite`: `an_existing_output_is_kept_unless_overwrite` and `a_race_for_the_output_is_lost_without_replacing` in `frame.rs`, and core's `persist_new` tests |

## odin's sixth run (H1 at 303e364)

odin's sixth run (PR #8, `hw-reports/odin-303e3644be.md`, at 303e364,
main with H1's fixes, before slice 7; the RX 7600, Mesa 26.2.5, FFmpeg
8.1.3, rustc 1.99.0 and kernel 7.2.9, the CPU governor on performance)
ran the whole of hw-test in 9032 s. Tier 2 passed 31 of 32 tests and
skipped `bench_playback_stitched_samples`, for want of clip 12. Tier 1
on the machine passed 662 and failed one.

- **The gates pass** on Vulkan Video, which Auto picks on odin's
  discrete GPU: 5.7K 8-bit and 10-bit for 60 s each, with no frame
  dropped, decode-dropped or late after the first second, and the 8K
  run (recorded) the same. Unthrottled, 5.7K 8-bit decodes 121 fps a
  second at the median (119 to 130), 10-bit 90 (60 to 98) and 8K 81 (67
  to 84), all past the gate's 35.96. The fifth run's 10-bit dip didn't
  come back.
- **X5 raw on Vulkan Video** drops nothing at 2.9K60, 8K25 or 8K30, and
  decodes 79 fps unthrottled at 8K30 (two 3840² lenses). At 1920² and
  120 fps it shows 60 a second, the display's rate, and the benchmark
  counts the other half as dropped.
- **VA-API with its copy is the wall**, as D25 found on huginn: in the
  engine its download takes 31 ms a frame at p50 for 5.7K 8-bit, 110
  for 10-bit and 122 for 8K, and those runs drop 30, 408 and 434 frames;
  at X5 8K30 it decodes 24 fps unthrottled and drops 249. That copy is
  what slice 5b takes away. One number went back since the fifth run:
  `bench_samples_realtime_decode` has VA-API at 0.60 × real time on the
  X5's 8K25 and 0.43 × on 8K30, against 1.38 and 1.04 at 43abcda, with
  the other modes and the synthetic decodes within 10 % of before. The
  media code between the two changed only the download pool's buffers,
  now on huge pages (H1-A), and that bench's decoders reuse two buffers
  each; odin's first run (PR #3) had runs of 8K30 and 2.9K60 at under
  half their median too. It was the code: with each of five allocators
  for the pool on odin, `av_buffer_alloc` played the 8K modes at full
  speed in 20 runs of 20 and every mapping at about half on most runs,
  with ten times the minor faults (`docs/perf.md`). The pool takes
  VA-API's buffers from `av_buffer_alloc` again, keeping huge pages for
  Vulkan Video's (m1-plan amendment 18). With the fix odin ran the 8K
  modes at 1.39 and 1.02 to 1.04 × in every run, and both 5.7K gates
  passed as before.
- **Seeks** on hardware: exact at 5.7K 126 ms median on VA-API and 119
  on Vulkan Video (245 and 244 p95), keyframe 55 and 45; an X5 8K
  lens's exact seek 182 and 183.
- **Uploads**: 5.7K NV12 through `write_texture` costs 2.6 ms of CPU
  and 1.0 of GPU a frame, 3840² NV12 pairs 5.9 and 0.9. The belt stays
  off (D31).
- **The Tier 1 failure**, `engine::playback_after_a_seek_waits_for_the_audio_preroll`,
  failed ten times in ten with the engine tests side by side and passed
  alone. It gave the upload thread 40 ms to bring in the frames after a
  seek's, which odin under that load didn't, so the clock waited for the
  video and didn't start at once when the audio was ready. With
  `--test-threads=16` it fails here too (3 in 3); the test now waits for
  those two frames by their times (on `m1/zero-copy`), and passes 10
  runs in 10 at 16 and 32 threads.

## odin's fifth run (H1)

odin's fifth run (PR #7, `hw-reports/odin-43abcdad28.md`, at 43abcda,
main after slice 6; the RX 7600, Mesa 26.2.5, FFmpeg 8.1.3, rustc
1.99.0 and kernel 7.2.9) was H1's first: Tier 2 passed 28 of 31 tests
in 8936 s, failed 2 and skipped 1 (`bench_playback_stitched_samples`,
for want of clip 12), and Tier 1 on the machine passed 605 and failed 4.
Branch `m1/h1-fixes` fixes all six failures and one pass that should
have failed, and odin's reruns of the failed tests over Remote Control
at its commits confirm each fix (the timelines are on branches
`hw/odin-timelines-2026-10-30` to `-30d`, with no PR):

- Four Tier 1 tests failed for the machine, not the code. On odin
  `auto` decodes on the GPU, and `pipeline_goldens`' 10-bit HEVC came
  back `p010le` where software gives `yuv420p10le` (frame's tests now
  decode in software unless they say how); `damage_is_said_once`, just
  opened with 16 frame threads, reached frame 13 by decoding on, so no
  landing was turned down (it seeks back from the end, and runs 16
  threads here too, failing so without the change); Fedora's
  libavformat loads samba's `*-private-samba.so`, which probe's
  confinement test took for a listed file (it looks for the listed
  files by their path now); and xtask's binding test asked
  `cargo metadata` for every platform's packages, Android's among them,
  which an offline cache that built the workspace doesn't have (it asks
  for the machine's). All four pass on odin.
- `bench_playback_x5_raw` passed, though every pass stalled at its
  second frame: both passes kept their first present, and its slots,
  for the whole run, and the X5's two lenses kept two of four slots, so
  neither stream could show its next frame. The passes let go of the
  first present once drawn, the Tier 1 harness test plays two streams
  and fails with the old hold, and x5_raw and the stitched benchmark
  fail when a pass stalls (D34).
- `bench_playback_realtime_5k7_10bit` dropped 181 frames after the
  first second (158 decode-dropped) and `_8bit` 14 (2). The benchmarks
  now write a CSV row per tick or frame (`SPHERECUT_PLAYBACK_TIMELINE`),
  and odin's timelines showed two causes:
  - At every loop point, the seek back to 0 left the cache holding the
    clip's end ahead of the playhead. The upload thread took the next
    frame by time, so those frames filled the slots that the frames
    after 0 needed, and the clock started with the seek's frame alone.
    The upload thread now follows a frame only with one linked to it or
    starting where it ends, and a seek's preroll waits, within its 2 s
    limit, until each stream has its next `PREROLL_FRAMES` (2) Ready or
    reaches its end (D34).
  - While the 8 GiB cache filled, every frame downloaded into a buffer
    just allocated, whose 4 KiB pages faulted in as the GPU's import of
    it pinned them, 12 150 a 5.7K P010 frame: 10-bit downloads took 40
    to 51 ms, against 18 to 21 once buffers came back to the pool, and
    Vulkan Video decoded 20 to 24 fps for the first 7 s. The pool's
    buffers are now 2 MiB-aligned mappings advised `MADV_HUGEPAGE`
    (D30): odin's cache then sat in 8.1 to 8.6 GiB of huge pages, a new
    buffer's download took about 14 ms and a reused one's 5 to 6, and
    10-bit's p50 over a run fell to 5.0 ms (24.5 at p95). The process
    still counts 12 150 minor faults a new frame, most likely because
    the kernel's import walks the buffer a 4 KiB page at a time and
    counts each page it touches, huge or not.
  - At 1991a98, Vulkan Video dropped no frame after the first second in
    any of the three runs, nor decode-dropped one, nor presented one
    late (202, 155 and 9 frames dropped at f87dc2a). 8-bit passes, and
    10-bit with a 1 GiB cache; with the default 8 GiB, 10-bit failed
    the unthrottled clause alone, at 58 and 49 fps and then 17 to 20
    fps for 3 s while the cache passed 5 GiB, its downloads taking 49
    to 70 ms, then 81 to 100 fps. A third run of both (8 GiB and 4 GiB
    caches) passed, the slowest second at 62 fps, with no compaction,
    no direct reclaim and no huge page falling back to 4 KiB, from 25
    GB free with 10 GB of it in free 4 MiB blocks.
  - The dip didn't come back in three more runs at the same code: with
    the 8 GiB and a 4 GiB cache from 25 GB free, 10 GB of it in free
    4 MiB blocks, and with a helper holding 10.5 GiB that left free
    memory in single pages, where the kernel compacted 395 times in the
    first 4.6 s; the slowest unthrottled seconds were 62, 62 and 48 fps,
    and compaction cost up to 4 ms a new frame's download (15 to 18 ms
    against 14). In the dip a new frame's download took 50 to 56 ms at p50 and
    a reused one's 5 to 6, as in every run, so it was the kernel's
    supply of new memory that slowed, for a reason not yet known (no
    `/proc/vmstat` was sampled then). It is a known limit, and the
    timeline and `/proc/vmstat` say what happens if it comes back.
- `audio_output_device_plays` passed on both of odin's outputs at
  855bb54: 182 ms median latency on Bluetooth A2DP, 64 ms on HDMI, the
  clock within 0.14 ms over 2 s and no xrun. Its ceiling is 400 ms now,
  for Bluetooth, since A/V sync follows any latency the host reports
  (D35).

The rest of the list passed: the adapter dump; the goldens on RADV and
on the host's llvmpipe; seek parity and latency on VA-API and Vulkan
Video; slice 4's `vaapi_render_matches_software` and
`vulkan_render_matches_software`; the samples' probes, decodes and
real-time decode; and `bench_playback_realtime_8k_8bit`, which records
only. The perf clips decode in software at 46.5 (5.7K 8-bit), 37.8
(10-bit) and 29.5 fps (8K), far from too easy (D37). On the X5's raw
files VA-API decodes 8K30's two 3840² streams at 1.04 × real time and
Vulkan Video at 1.93 ×. Two decisions follow:

- **The staging belt stays off** (D31): `bench_upload_5k7` at 43abcda
  has `write_texture` making no allocator block at any input, and the
  belt never cheaper. One step in flight, 5.7K NV12 costs 2.57 ms a
  step against the belt's 15.84; P010 4.31 against 4.32; the 3840²
  pairs 5.83 against 5.86 (NV12) and 9.26 against 9.31 (P010). The
  trigger now asks the belt to be cheaper. huginn hasn't run H1, and
  its numbers can still turn it on.
- **U2 isn't needed on odin** (m1-plan 4.4): its trigger is download
  plus copy over a third of a 30 fps frame, 11.1 ms. At 1991a98 and
  after, 10-bit's Vulkan download and `write_texture` take 5.0 and 5.9
  ms at p50 (8-bit 2.6 and 3.0), on two threads, and nothing drops.

## odin's fourth run

odin's fourth run (PR #6, `hw-reports/odin-918d03171c.md`, at 918d031,
main after the golden fixes and the seek fixes; the RX 7600, Mesa
26.2.5, FFmpeg 8.1.3 and rustc 1.99.0 of PR #5) passed all 25 Tier 2
tests in 4124 s, among them slice 4's `vaapi_render_matches_software`
and `vulkan_render_matches_software` (`--decoder auto` included) and
the two golden tests that failed in PR #5:

- `goldens_on_gpu_match_reference`: every CPU check is the number D32's
  emulation gives (its table's "Emulated now") to the hundredth of a
  dB: `rect_forward` 59.10, `rect_ypr` 58.90, `fisheye_180` 58.90,
  `fisheye_360` 59.93 (44.67 at be38fa4), `stereo_tinyplanet` 58.90,
  `morph_half` 58.88, `crop_half` 60.75 and `matrix_601_709` 58.91 and
  58.95 dB, 8.57 to 11.78 dB above lavapipe's floors. The prediction,
  which added the squared error the emulation missed at be38fa4 as
  RADV's own, came out 0.26 to 3.55 dB low (2.79 and 3.55 for the
  fisheyes), so none of that error was RADV's filter: it went with the
  old mips or the old encode pass. The LOD bands are the emulation's too
  (`fisheye_360` 59% of its error from 1 to 2.5 at 58.90 dB and 41% from
  2.5 at 58.86; `fisheye_180` 92% and 8%, at 58.90 and 58.92). Every
  seam, pole, shimmer, ramp, range and siting row passes with its
  control failing, as in PR #5, and D32's Revisit clause doesn't hold.
- `passthrough_exact` on RADV: limited 86, 159 and 165 one code off the
  formula, as on lavapipe, nothing off the model and full range exact;
  the ten codes RADV's pack put a code low in PR #5 are right (D31).
- `goldens_on_host_llvmpipe` (LLVM 22.1.8): no pixel more than 4 LSB off
  any of the six stored images (`golden_pixels_past_4_lsb` 0 at each),
  at most 4 LSB off, at 63.17 to 68.39 dB and SSIM 0.9999 or more.
  `fisheye_180`, 5 LSB off in PR #5, is 4 off since the stored images
  moved with the mip fix. The rule's count of 4 stands against a drift
  of 0 there (D32).

Decode, seek and projection repeat PR #5's within about 10%, but for
VA-API's X5 rows, up 4 to 30% (8K30 at 0.95 ×, still under real time,
where Vulkan Video does 1.92 ×). Two of the upload numbers moved
(`docs/perf.md`):

- The mips of a 5.7K NV12 frame take 1.01 ms on the RX 7600, against
  0.62 ms at be38fa4, and the 3840² pair's ingest and mips 0.89 ms a
  frame against 0.59: the cost of the mip pass working its 8-bit means
  out in integers. P010's mips (0.51 ms) didn't move, since deep planes
  keep their f16 path.
- `write_texture`'s CPU time a step fell 28 to 37% at 8 bits (5.7K NV12
  2.58 ms, against 4.12; the 3840² pair 4.61, against 6.42) and about
  10% at 10 bits, with no change to the upload between the runs, and
  the belt's 3840² pair fell as much (6.69 to 4.67 ms): not yet
  explained. The belt's 5.7K NV12 step, which at one step in flight
  makes or frees allocator blocks every time, barely moved (16.26 ms,
  against 17.39; 3.68 ms at two in flight). So the plan's CPU trigger, which held for both 5.7K
  inputs in PR #5, holds now only for P010, through its 4 ms arm (4.34
  ms), where the belt costs the same (4.36 ms). As written it would make
  the belt the default where the belt is no cheaper, and at 8 bits, one
  step in flight, six times dearer; H1 decides it with huginn's numbers
  (D31, Next).

## odin's third run and slice 4's checks

Checks of `m1/frame` on 2026-10-28, on code unchanged since ff97714 (the
merge of main; only docs changed after it): `cargo xtask check` passed
three times, the last two in a row on the branch's last commit;
`TimeSpec`'s proptests passed at `PROPTEST_CASES=100000` (2 s); `cargo
+1.95 build --workspace --all-targets` built with no warning (3 min 38
s); and `cargo xtask compat` passed clippy and the link against FFmpeg
8.1.3 (libavcodec 62.28.103, as Pat's) and 9.0.2, and Tier 1 against
8.1.3, the new frame tests included (8 min 12 s). `cargo xtask hw-test`
lists the two new Tier 2 tests.

Checks of main at 498d6e1, the merge of all three branches: `cargo
xtask check` passed, and `cargo xtask compat` passed clippy, the link
and the binding against FFmpeg 8.1.3 and 9.0.2, and Tier 1 against
8.1.3. A last review of the decoder reopen and the landing rule
(hardware paths, the merge's interactions, exact seeks) found no wrong
frame and three minor faults, fixed on `m1/seek-nits`, each with a test
that fails without its fix (D29, D30):

- A file trimmed by stream copy mid-GOP (an edit list that hides its
  first IDR picture's frame) reopens the decoder at every seek that
  lands on its first packet, though a flush gives the same frames:
  20 to 25% more time on those seeks, measured in software. Fixed in
  1748d3a: the first packet's NAL units say whether its picture is one
  FFmpeg flags a keyframe; the new test's first clip reopened 5 times in
  5 seeks without the fix, and none with it (99f2d6b counts reopens in
  `DecodeStats`).
- A reopen that fails (in practice only out of memory) loses the
  stream's first packet and is taken for the file's damage. Fixed in
  4289173: on a device it is the device's failure, and the fallback goes
  on in software; in software it is an error until a seek. Injected,
  it had made an exact seek to frame 2 return frame 6 as `Exact`.
- When the landing rule turns down a landing, damage met while decoding
  to its first frame is said again by the decode from the earlier
  landing: two `Damaged` events, and two warnings in `spherecut frame`,
  for one damaged packet. Fixed in 62e3be4: damage is said once from one
  seek to the next, however often decoding meets it, which also ends two
  older doubles: a keyframe seek past a cut said the cut and playing on
  to it said it again, and the step after a stopped seek said playback's
  damage again.

Those tests found that the oracle fails a closed-GOP clip behind an edit
list of 3 frames, H.264 or HEVC, at frame 13: an exact seek to a
keyframe right after a keyframe seek there seeks again where decoding
on was cheaper. Its frames are right, and it was so before the fixes
(D29).

Checks of `m1/seek-nits` at 56d9200: `cargo xtask check` passed;
`nal`'s three proptests passed at `PROPTEST_CASES=100000` (10 s); and
`cargo xtask compat` passed clippy, the link and the binding against
FFmpeg 8.1.3, and Tier 1 against it, the new tests included (5 min).

The review of `m1/seek-nits` found one more minor fault, older than the
branch (the same at 92db604), and it is left as a limit for now (D29):
where FFmpeg starts an H.264 picture from a damaged slice header and
then fails its slice, it hands the picture out with whatever its buffer
held, and the frames after it, up to the next IDR picture, depend on
what the decoder decoded before. An exact seek there returns frames
unlike the decode from the start's as `Exact`: 9 frames of a 40-frame
clip, on 6.1, and plain FFmpeg does the same on 6.1, 7.1.3, 8.1.3 and
9.0.2. Only FFmpeg's log says so, unless another packet fails. Once
said, it is damage before the target like any other (the plan's rule
and D27's); saying it means reading FFmpeg's log per decoder, which
comes after slice 5 (Next, item 2).

odin's third run (PR #5, `hw-reports/odin-be38fa409e.md`, at be38fa4):
21 of 23 Tier 2 tests pass, including all four that failed in PR #4. Two
golden tests fail, for three reasons, and `m1/goldens-hw` settles all
three, each by a fix (D31 and D32, amended 2026-10-28; m1-plan
amendment 13):

- `goldens_on_gpu_match_reference` on the RX 7600:
  - `fisheye_360` is 44.67 dB against the CPU reference, below its 46.59
    dB floor, which was calibrated on lavapipe (measured − 3 dB).
    `fisheye_180` (47.78) and `stereo_tinyplanet` (51.45) are lower on
    RADV too, while `rect_forward` is higher (58.5). Found in 5f134c5,
    e8a8dc0, 2ef35f0, f356e29 and 260e13a: an emulation of the CPU
    reference puts the cause not in the GPU's filtering precision
    (8-bit fractions with a blend of 4 bits cost any check at most 1.93
    dB) but in the mip pass's 8-bit stores, left to the driver: RADV's
    export put every mean on a half code a code low, and the levels
    compounded it (luma level 4 a mean 1.13 codes low on the grid),
    against a reference whose own mips took ties up and ran high.
    Emulated, that reproduces all nine of the RX 7600's CPU checks
    within 0.43 dB. For a while a GPU's CPU checks were held to the RX
    7600's numbers less 3 dB (`fisheye_360` 41.67). Fixed in 9d2d218,
    5fb9535, d0ef333, df4a668, 1aa67f0 and 5583d9d: the mip pass works
    each 8-bit mean of codes out exactly in u32 (no overflow at 16384 a
    side) and rounds it to the nearest code, ties to even, as
    `reference::halve_codes` does, and on lavapipe every texel of every
    level of R8 and Rg8 planes (5 to 8 a plane) at even and odd sizes up
    to 1024×512, and of level 1 at 16383 and 16384 a side, is the
    reference's, code for code (D31). The lower floor is gone: a
    GPU is held to lavapipe's floors, and the notes break each CPU
    check's error down by LOD band. The emulation predicts the RX 7600
    after the fix at 56.11 to 59.78 dB (`fisheye_360` 56.38), 7.91 dB
    or more above them (D32). Six stored goldens and `h264_full_range`
    moved by up to 4 LSB, only where a pixel reads level 1 or more and
    nearly all down (lavapipe's own export had put the mips high), and
    were blessed again through the gate: all nine CPU floors of the
    render goldens rose (`fisheye_360` 46.59 to 48.15 dB, `fisheye_180`
    47.01 to 48.2) and `h264_full_range`'s (47.2 to 48.17), the other
    three pipeline goldens' were unchanged, and `stereo_tinyplanet`'s
    ideal floor fell 0.01 dB. RADV's numbers came
    with odin's fourth run (above).
  - `passthrough_exact`: 11 limited-range codes (74, 86, 129, 135, 141,
    153, 159, 208, 214, 220 and 226) come out one code low, where the
    model of the chain predicts only 86, 159 and 165, and three of them
    are outside its f16 bound. The likely cause is RADV's colour export
    to an 8-bit target, which packs the shader's output to f16 with
    round-toward-zero before the unorm conversion. Fixed in 94ee458,
    bb896e7, da6ab34 and d29fb6f: the encode pass rounds to the target's
    codes itself and returns k / 255, which packed toward zero stays
    within 0.1245 codes of k, so no integer target is needed (D31);
    after the branch's review (99a8cac), it returns the nearest half of
    k / 1023 for `Rgb10a2Unorm`, within 0.25 codes of k however it is
    packed, where k / 1023 packed toward zero came within 0.0005 codes
    of a tie, and the OETF's nearest half into `Rgba16Float`. A unit
    test emulating the pack on the unrounded values gives exactly the
    ten codes odin measured off the model, and none once rounded. No
    golden moved on lavapipe, so nothing was blessed again; RADV's
    numbers came with odin's fourth run (above).
- `goldens_on_host_llvmpipe` (LLVM 22.1.8, Mesa 26.2.5): `fisheye_180`
  differs from the PNG blessed on LLVM 20.1.2 by at most 5 LSB, one over
  the other-driver rule's 4, at 62.79 dB and SSIM 0.99994. Fixed in
  4e341df, ccf38e5, 17425be, 0bb7603 and c1049b5: the rule keeps PSNR ≥
  48 dB and SSIM ≥ 0.995, and its single-pixel maximum becomes at most 4
  pixels with a channel more than 4 LSB off and none more than 8, for
  `goldens` and slice 4's `pipeline_goldens` alike. A new Tier 1 test
  makes local faults from every stored view (moved a pixel, a pixel
  wider or narrower, the seam, the poles or the rim blacked out,
  implicit LOD): each fails the rule wherever it shows (implicit LOD
  doesn't in the rect views, which are within 4 LSB), and the four the
  averages miss are caught by the count (implicit LOD, 11 to 72 pixels
  past 4 LSB) or the maximum (a black pole quad, 95 LSB). Drift made
  here (lavapipe's float filtering, views moved a hundredth of a pixel)
  puts at most 1 pixel past 4 LSB and at most 5 LSB. `judge` reports the
  count and the maximum, and every run notes `golden_pixels_past_4_lsb`,
  so odin's next run records LLVM 22's count (0 at every row in its
  fourth run, above). Both manifests were
  blessed again for the new thresholds through
  `SPHERECUT_UPDATE_GOLDENS=1`; no floor and no PNG changed.

The branch's review found 13 things, all fixed or corrected (b741dec,
99a8cac, 75fcd95, 0d5fb38, 31acdde, b15c1cd and this file):

- The fault test of the other-driver rule read each stored PNG from
  `tests/goldens`, which `goldens` rewrites while blessing, beside it:
  after a renderer change that moved a stored image past the rule, the
  blessing command wrote everything and exited 101, as the Tier 2
  paths' tests once made it. It now renders the views; with a stored
  image changed under a private mount, the blessing passes (D32).
- The encode pass returned k / 1023 into `Rgb10a2Unorm`, which a pack
  to f16 toward zero puts as little as 0.0005 codes above a tie (if
  RADV packs its 10-bit exports as it does its 8-bit ones, which
  nothing has measured), and the OETF unrounded into `Rgba16Float`,
  whose half lavapipe truncates (471 of 1020 values a half low). It
  returns the nearest half in both now (D31).
- `the_wide_views_lose_to_the_mip_stores_not_the_filter` held the
  machine's lavapipe to the model of LLVM 20's 8-bit filter, and failed
  under `GALLIVM_PERF=no_aos_sampling` (5.53 dB off); the model is now
  asserted only where a probe finds lavapipe filtering 8-bit planes to
  whole codes, as here (D32).
- `the_other_driver_path_runs_on_lavapipe` passed with the exact rule
  in place of the other-driver one, or without the count; it now
  changes its golden so that only the right rule passes it. The CLI's
  rule is a function with a test of its own (D32).
- The docs said the floor change fixed the RX 7600's loss, that LLVM 22
  put one pixel 5 LSB off (only its maximum was recorded), that a
  filter's precision explains 1.9 dB of `fisheye_360`'s loss at most
  (4.46 dB with a 4-bit blend toward zero), that implicit LOD along the
  seam fails SSIM, that the labels' ink is at 1 (it is at 0, and 171 and
  28 pixels sit at 5 to 8), an RMS of 0.19 LSB (0.18), the wrong faults
  as those the averages miss (ARCHITECTURE), and that a GPU's CPU checks
  are held to lavapipe's floors only (perf.md, D32's rules table).

The mip fix's review found 6 more, all fixed or corrected (cf50f0d,
3586586, 16e4bf9, e8d37c1, 1b6ae07 and the commit that adds this list):

- The emulation's control said the old means through RADV's export fall
  under the floors blessed now, but measured them against be38fa4's
  reference. Against today's they clear every floor (`fisheye_360`
  49.02 dB predicted, against 48.15), so a GPU run that only clears the
  floors can't tell the fix from the old mips. The control is now held
  to the prediction, which the old means miss by 1.81 to 7.36 dB
  wherever a view reads level 1 or more, and H1 holds the RX 7600 to it
  (D32, Next).
- The docs and comments said Vulkan makes every driver keep k / 255 as
  k. That holds for a driver that converts to nearest, which Vulkan asks
  for but doesn't require; Vulkan's letter guarantees it only for an
  exact integer, which k / 255 times 255 is only in f32 and with a
  correctly rounded division, which WGSL doesn't promise (D31).
- D32's pipeline goldens table and this file gave `h264_full_range`'s
  numbers from before it was blessed again (now 51.2 dB, floor 48.17);
  the docs said every CPU floor rose, where three pipeline goldens'
  didn't; D32 said the emulation test reproduced its be38fa4 table's
  filter in f64, which it printed only against today's reference (it
  prints both now); and D31 and this file said the code-for-code test
  covers 8 levels at every size up to 16384, where the odd noise frames
  have 5 to 7 and the largest sizes level 1 only.

## Slice 5: the headless playback engine

Built on `m1/playback` and merged into main on 2026-10-29, in three
parts: the clocks, the UI thread's guard and the frame cache; the
engine; and the Tier 2 playback benchmarks with hw-test's H1 additions
(D34, D37). The slice's acceptance (m1-plan
section 14):

| Acceptance | Evidence |
|---|---|
| The cache's proptests and the budget's fixtures pass | `the_cache_keeps_its_rules`, against a model of the cache's rules, at `PROPTEST_CASES=100000` (27 s in a release build; 24 s with pins, since the review); it caught each of eleven faults put into the cache by hand. `a_system_clock_adds_up` at 100000 too. `playback::budget`'s fixtures: huginn's 128 GB take the 8 GiB cap, odin's 32 GB a quarter of 24 GiB available, a small machine the floor, a cgroup v2 limit up a fake tree caps it, a namespaced container's at the root, page cache the cgroup would give back is room, and rechecks under pressure settle (D34) |
| `never_blocks` (the engine's part), the seek storm and shutdown pass | `crates/playback/tests/never_blocks.rs`, on a thread marked with `OnBlock::Panic`: with a backend that never answers, UI iterations took 179 µs at the 99th percentile; through 1000 seeks in 2 s, 199 µs, with one of the 1001 requests decoded to completion and the last seek's frame shown; a player stuck at the gate joins cleanly in 12 to 160 ms; the negative control panics. Five runs in a row beside 4 busy loops (D34) |
| A 256 MiB-budget 5.7K run plays without exceeding its budget | `crates/playback/tests/engine.rs`: 60 frames of 5760×2880 through a 256 MiB cache and four slots, every frame shown in turn, none dropped, the cache peaking at 237 MiB with no "too small" notice (D34) |
| H1 with the list in section 12.3 | Run on odin (PR #7) and fixed after it ("odin's fifth run"); huginn's is to come (Next, item 1) |

The M1 criterion "decode, audio and texture upload on workers; UI never
blocks on them" stays unticked: the engine's part is in, and audio's
(slice 6) and the GUI's (slice 7) are to come. "5.7K30 stitched
equirect plays in real time" waits for huginn's H1: on odin both 5.7K
gates pass since the H1 fixes but for one 10-bit run ("odin's fifth
run"). Until Pat's clip 12 comes, a pass there is on synthetic content
at camera bitrates, and the
criterion is "done pending hardware" for real stitched footage (plan
section 12.3).

The third part:

- `crates/playback/tests/tier2_playback.rs` has six Tier 2 benchmarks,
  each `#[ignore = "tier2: …"]` and run by hw-test, and one Tier 1 test
  of their harness (D34, `docs/perf.md`):
  - `bench_playback_realtime_5k7_8bit` and `_10bit` play the 5.7K perf
    clips and gate on `Auto`'s hardware decoder: no drop and no
    decode-drop after the first second of 60 s in real time, every
    unthrottled second at 1.2 × 29.97 frames or more, and no fallback to
    software. The other hardware kind and software are recorded;
  - `bench_playback_realtime_8k_8bit` and `bench_playback_x5_raw` (the
    longest X5 file of each mode, both lenses) record;
  - `bench_playback_stitched_samples` gates a stitched sample of 5.7K at
    30 fps or less as the perf clips are, at 1.2 × its own rate;
  - `bench_perf_clips_software_decode` records the rate hw-test flags
    as too easy over 150 fps.
- How they skip, each with `TIER2-SKIP` and its reason: outside hw-test
  (no `SPHERECUT_TIER2`); no GPU, or a CPU adapter such as llvmpipe;
  the perf clips not made (`--quick`, or `SPHERECUT_PERF_MEDIA` unset)
  or one missing; no samples, or none stitched; and for the gated
  benchmarks, `Auto` picking software, whose reason the skip quotes,
  after the software run is recorded. A skip is never a pass.
- hw-test (D37) makes the three perf clips once into
  `<target>/hw-media/` and reports their size, bitrate, how they were
  made and their software decode rate; runs Tier 1 on the machine after
  the Tier 2 tests (`--no-tier1` leaves it out), where a skip fails;
  reads the power settings and the render nodes; checks rustc 1.95, the
  lavapipe ICD and the ALSA headers; and lists the Tier 2 tests with
  `--list`.

Evidence for the third part, on this box:

- `the_bench_harness_plays_on_lavapipe` (Tier 1) plays a second of
  30 fps H.264 through both passes on a marked thread: all 29 frames
  after the first unthrottled, 2 real-time seconds, two loops in
  2.5 s, and the upload, render and UI stages gathered. It passed three
  runs in a row alone and three beside 4 busy loops. Run once by hand
  with doctor's checks faked to say Vulkan decodes H.264, the gated
  path recorded every row and failed the gate as it should, naming the
  fallback and the drops by second.
- With `SPHERECUT_TIER2` set and the X5 samples, every benchmark skips
  with its reason, the GPU's being "no GPU: the adapter
  SPHERECUT_ADAPTER=auto picks is a CPU, llvmpipe (LLVM 20.1.2, 256
  bits)".
- `cargo xtask hw-test --list` lists 31 Tier 2 tests, the six
  benchmarks among them. xtask's 63 tests pass, among them Tier 1 read
  from real cargo output and the perf clips made, cached and made again
  on a tiny spec.
- hw-test made the 5.7K 8-bit perf clip here in 26 min 52 s: 254 MB at
  101.6 Mbit/s, which FFmpeg 6.1 decodes in software at 27.1 fps on 4
  CPUs, far below the 150 fps that would make it too easy (D37). The
  run was stopped before the other two clips.
- The part's first `cargo xtask check` failed an engine test of the
  second part, `frames_passed_over_while_playing_count_as_dropped`, in
  3 of 19 runs of its binary; one of those also failed
  `playback_after_a_seek_waits_for_the_audio_preroll`. The first waited
  for a frame without presenting while the cache kept a playhead the
  presenter had lost to a taken lock; the second read a snapshot that
  was the copy from before. The tests now present while they wait and
  read fresh snapshots, and the presenter asks for a repaint when it
  loses a playhead, with a test that fails without it (D34). Then 20
  runs of the engine binary passed, and 5 of each playback binary
  beside 4 busy loops.
- `cargo xtask hw-test --quick` with the samples, end to end here,
  failed its Tier 1 run at first on xtask's own cargo-output test,
  which inherited hw-test's terminal settings, and the scratch cargo
  output its assertion printed read as 8 binaries of the run's own.
  Fixed, with a test (D37); the run then passed: Tier 1 589 passed in
  43 binaries (115 s), Tier 2 2 passed and 19 skipped (no render
  node), and every new section of the report written ("Power profile"
  and "Render nodes" say "not available" and "None" here).
- Checks of `m1/playback` at 11a9d32 (only docs changed after it):
  `cargo xtask check` passed (2 min); no proptest is new or touched in
  this part, and the slice's two passed at `PROPTEST_CASES=100000` in
  its first and second parts; `cargo +1.95 build --workspace
  --all-targets` built with no warning (2 min 43 s); and `cargo xtask
  compat` passed clippy, the link and the binding against FFmpeg 8.1.3
  (libavcodec 62.28.103, as Pat's) and 9.0.2 (63.1.102), and Tier 1
  against 8.1.3 (8 min 5 s).

The review's fixes, on this box (D34, D37):

- Each fault's test, where one can show it, was run with its fix
  taken out and failed (D34's "Tested" says how). The playback tests,
  73 passing and the 6 Tier 2 benchmarks ignored, passed five runs in
  a row (20 s each), and the engine's, `never_blocks`' and the cache's
  40 three runs in a row beside 4 busy loops. xtask's machine tests
  passed 200 runs in a row once each fixture tree had a directory of
  its own.
- `the_cache_keeps_its_rules`, touched for the pins, passed eight runs
  in a row at `PROPTEST_CASES=100000` in a release build (24 s each).
  Runs before that found the model wrong in four ways only a pinned
  frame past the budget reaches; the cache was right each time, and
  `Insert::Dropped`'s doc said less than it does (D34).
- `cargo xtask check` passed at a667f82, after which only this file
  changed (2 min 12 s, 601 tests passed). No FFmpeg-facing code
  changed, so `cargo xtask compat` wasn't run again.

A fresh re-review of the fixes (93625f4 to 297c739) found one must-fix
and four minor faults; the tests written for them turned up two more.
All are fixed, each with a test that fails without its fix (D34):

- **Must-fix: a keyframe seek back on two streams could stay in
  Seeking.** With fewer than two slots a stream the presenter shows
  each stream's new frame as it comes; the first stream's landing was
  unpinned once in a slot, then evicted as the frame farthest behind,
  and its stream wanted nothing, keeping its old frame's slot from the
  other. A landing now stays pinned while its request is the target.
  Two tests (three slots, and four with one Dead, the cache full)
  stuck at [40, 60] before.
- The branch where a stream that failed the request wants no slots had
  no test: the re-review's mutation M3 passed every test. A test now
  fails the seek of the stream whose next frame holds the third slot.
- A step during a keyframe seek started from the time asked (46 after
  `seek(45, Keyframe)`, where the seek shows 40). It now starts where
  the seek would land.
- The budget's recheck swung under pressure (512, 358, 512 MiB every
  5 s with 1.4 GiB available). It now leaves 1 GiB of what the cache
  and the rest have together, at least a quarter of it, which the
  cache's evictions don't change; this caps an explicit size and a
  container's room too.
- A cgroup's room counted page cache as use: a container reading a
  large file had a budget of about 2 MiB. Its use is now the working
  set, `memory.current` less `inactive_file`.
- Two more, found by the new tests. A step superseding a seek before
  its worker started it could wait for cache room for good, the
  playhead left at 0 (2 of 40 runs). And on three slots one lens's
  next frame could take the slot the other's frame at the playhead
  needed, the lenses then showing different frames (16 of 16 runs of
  the new test under the old rule, and 2 of 12 suite runs of the
  keyframe test).
- Left as a documented limit: with no more live slots than streams
  (two Dead of four, on two streams) a seek or a step never lands.
  Slots die only on GPU out-of-memory; the viewer is to treat that as
  a GPU failure (Known issues, D34).

Evidence for these fixes, on this box: the playback tests (81 run, the
6 Tier 2 benchmarks ignored) passed five runs in a row, and three
beside 4 busy loops; the engine's 26 passed 10 runs in a row after the
last fix. `cargo xtask check` passed at ce474d0, after which only the
docs changed (2 min 12 s, 609 tests passed). No proptest was touched:
the cache's changes are to its docs.

## Slice 6: audio

Built on `m1/audio` (D35): audio decoding in `media::audio`, the
`spherecut-audio` output crate, and the engine's audio. The slice's
acceptance (m1-plan section 14):

| Acceptance | Evidence |
|---|---|
| The audio decode tests pass | `crates/synth/tests/audio.rs`, 13 tests (26 s) on clips synth writes: exact sample counts; the tones at 440 and 660 Hz; 44.1 kHz resampled to exactly 10 s at 48; priming gone and the audio on the video's axis; seeks at five times, sub-frame and 1001/30000 among them, starting at round(at × rate) with a sequential decode's samples within 1e-6, at 48 kHz and from 44.1, in MP4 and in a Matroska remux; mono, 5.1, ambiX and the output's channels; origins before and after the audio; a file without audio (`no_stream`); a Matroska remux with every sample of the MP4's from the first, within a millisecond, at 48 and 44.1 kHz; and `sync-flash-beep`'s beeps within 1 ms of their white frames |
| The fake-device A/V tests pass at all four rate combinations | `crates/playback/tests/av_sync.rs`, 10 tests (5 s) through the whole engine, most on `FakeAudioDevice`: `video_follows_the_audio_clock_at_every_rate` plays 60 s of scripted time at 29.97 and 59.94 fps with 44.1 and 48 kHz, the frame shown at every present the one whose interval holds the clock's time; a seek restarts audio at round(at × rate); a 200 ms decode stall leaves the clock running, video continuous and the sample heard after it the clock's; losing the device moves to the system clock without a jump over 1 ms; 20 callbacks 30 ms late never take the time presented back; a preroll waits its 2 s limit; a file without audio plays on the player's clock; and the audio's threads end with a newer open and the player's join, which reports them |
| NullOutput keeps time on this box | `the_null_output_keeps_time` in `av_sync.rs` (within ±5% over 2 s through the engine) and in `crates/audio/tests/clock.rs`; `the_default_device_is_the_null_output_without_one`: with no device here, `AudioDevice::Default` plays on the null output |
| Tier 2 `audio_output_device_plays` is written | `crates/audio/tests/tier2_device.rs`, listed by `cargo xtask hw-test --list` (32 Tier 2 tests). Here it skips with ALSA's "The requested audio device is not available"; its measuring, run once by hand on the null output, gave 1.99992 to 1.99999 s of clock over 2 s and a worst callback gap of 1.009 buffers in three runs. On odin at 855bb54 it passed on both outputs, PulseAudio's protocol to PipeWire, f32 at 48 kHz, 1024-frame buffers: on the Edifiers over Bluetooth, 2.0000096 s of clock over 2 s, 181.9 ms median latency (169.6 to 182.0), worst gap 1.0056 buffers; on HDMI, 1.9998605 s, 64.1 ms, 1.0051; no xruns. Its latency ceiling is now 400 ms (D35) |

Also in the slice:

- The audio crate's own tests: 10 unit tests (the ring's placement,
  epochs and reclaim, a two-thread stress test and a reclaim racing an
  epoch's first fill; the clock's rounding, packing, floor and limit;
  `audio-out`'s stall rule) and the 13 of `tests/clock.rs` (2 s), most
  on the fake device (D35's "Tested").
- The slice's review (a fresh subagent's, against the acceptance
  above, which it found met) found 8 faults, all fixed (D35's
  "Tested"): a stalled stream opened again every 50 ms; a reclaimed
  chunk read as a gap; the time presented going back up to 9 ms after
  a late callback; Matroska's end cut taking 16 samples off each of the
  first frames, with `place()` untested; the audio's threads outside
  the player's shutdown and join; the probe's interrupt never set; the
  null output anchoring at its wakeups; and D35's grid point the wrong
  way round.
- `cargo xtask check` stops, with the install command, when pkg-config
  finds no `alsa`; hw-test's prerequisites say ALSA's headers block the
  build. `deny.toml` allows cpal only `pulseaudio`. cpal's D22 row:
  20 crates more in a Linux build, 44 in `Cargo.lock`, and two more
  duplicates (thiserror 1, and jni-sys within the Android tree).
- `PlayerEvent::AudioOpened`, `NoAudio` and `AudioLate`, and
  `Player::audio_stats()`, for the GUI's notices and the overlay
  (slices 7 and 8).

The M1 criterion "decode, audio and texture upload on workers; UI never
blocks on them" stays unticked until the GUI's part (slice 7): the
audio's threads are in, and nothing of theirs runs on the UI thread,
which reads the clock through its seqlock and the output's numbers
with `try_lock`.

## Slice 7: the GUI viewer

Built on `m1/gui` (D33, D36): `spherecut-app`, eframe 0.37 on render's
own device, and `spherecut` with no command opening it. The slice's
acceptance (m1-plan section 14):

| Acceptance | Evidence |
|---|---|
| The whole kittest suite passes on lavapipe, the byte-identical fractional-scale test, the marker drag at 1.25 and GUI `never_blocks` included | `crates/app/tests`, 16 tests (about 30 s). `viewer.rs`: the viewer's crop of the harness render is `render_still` byte for byte at 1, 1.25, 1.5 and 2 pixels a point, with the same view and filter; the pointer maps through the viewport egui-wgpu gives the callback, and `prepare` made its target that size (at 1.25 and 1.5 and odd sizes, rect corners on whole pixels, lens points to 1e-5); the marker drag at 1 and 1.25 lands the red marker's centroid 0.09 and 0.34 px from the release point, with the app's q `solve_level`'s to 1e-9; zoom by wheel and `zoom_delta` keeps the direction under the pointer to 1e-9; the switch takes 15 frames at 60 Hz; an unchanged frame and view only encode, and a resize projects once; steps show the next and previous frames and Home and End the ends; a resize sweep from 1×1 to 3840×2160 never panics; a projection `prepare` recorded but `paint` never followed (a failed surface acquire) is projected again; and with `logic` alone, as eframe runs a hidden window, playback presents frame after frame and ends the clip, which nothing ends without it. `never_blocks.rs`: above, under "M1 criteria ticked". `panes.rs`: the doctor's errors and warnings are banners and its notes aren't, Dismiss keeps one gone, the Doctor tab shows the whole report and, before it, "Checking codecs and hardware…"; lavapipe's own warning with its fix; the Files pane lists a folder (folders first, videos only, hidden files left out), opens a clip and goes in and up; and the notices for a file without audio, a null output (dismissed, it stays so across the next open) and a hardware fallback |
| `cargo deny check` is green with only the font exception | advisories, bans, licences and sources pass with no ignores; the one licence exception added is `epaint_default_fonts`'s (Pat's answer, D36). The graph is Linux's only, which leaves out arboard's Windows and macOS image codecs |

Also in the slice:

- `crates/app/clippy.toml` bans the blocking calls on the UI thread
  (D36), with the player's `join` added to the plan's list, and every
  `#[expect]` is fulfilled.
- The device: `Gpu::for_window` and egui-wgpu's `WgpuSetup::Existing`
  (D31), so slice 5b's device reaches the viewer.
- `media::probe::render_text` and `media::doctor::render_text`, moved
  from the CLI, for the Info and Doctor panes.
- `FrameCache::try_ranges`, the scrub bar's cached spans, which the plan
  had in slice 8.
- `no_display`, exit 3, joins the CLI contract (`docs/cli-json.md`).
- Departures from the plan's section 7, recorded in D36 and amendment
  16: the viewer's rect is inset by a pixel, the redraw is a `DrawKey`
  `prepare` compares rather than a flag (a projection counting as drawn
  once `paint` ran), `logic` presents so playback goes on while the
  window is hidden, the jobs' threads are joined at exit, and the
  marker drag test looks through the fisheye.
- The fresh review's eight findings were fixed in the slice: a
  projection whose surface acquire failed stayed unprojected; nothing
  presented while the window was hidden; the jobs' threads were left
  running at exit; a focused text field kept the scrub's exact seek
  back; machine notices came back undismissed on each open; two tests
  checked less than they said; docs claimed more than the code did; and
  the viewer polled the device on the UI thread.
- The GUI adds 90 crates to a Linux build (D22).
- Slice 8 has the overlay's GPU timestamps and sparklines (slice 7's
  overlay is the engine's numbers as text), the settings beyond the
  layout, the raw view for sources that aren't equirect, and `--smoke`.

## Slice 5b: zero copy, path A

Built on `m1/zero-copy` (D25, `docs/design/m1-5b.md`): VA-API frames
mapped to DRM PRIME and their planes imported into wgpu's device, one
texture a plane, then copied GPU to GPU into the slot (8-bit) or run
through the ingest pass (10-bit), as the copy path's planes are. Off by
default; `SPHERECUT_ZERO_COPY=1` turns it on in the viewer, and
`spherecut frame --zero-copy` for one frame.

- **media**: `VideoRequest.output` (`HwOutput::Download` or
  `DrmPrime`), `VideoFrame::dmabuf()` (a layer a plane: fd, offset,
  pitch, modifier, fourcc), `to_system()` for a reader that can't
  import, and `VideoFrame::transfer`, which says how a frame left its
  decoder. A frame that won't export as one plane a layer (a compressed
  modifier, a crop on the left or top) is downloaded, and its transfer
  says why. The decoder keeps `ZERO_COPY_SURFACES` (24) surfaces beyond
  its own.
- **render**: `render::dmabuf`, render's one `unsafe` module (D28): an
  import's plan from the layers, and the import through wgpu-hal's
  `texture_from_dmabuf_fd`, after asking the driver through ash whether
  it takes the format's modifier from a DMA-BUF, and how large
  (`import_failed` when not). `Uploader::upload_imported` and
  `StillRenderer::render_imported`. `GpuCaps.dmabuf_import`.
- **playback**: `PlayerConfig.zero_copy`. The upload thread imports
  each frame afresh (a cached import would see no barrier when the
  decoder writes the surface again) and keeps it until the GPU work
  that read it is done. A refused import downloads that frame, is kept
  in the snapshot, and the stream's later opens ask for downloads. The
  frame cache holds at most `SURFACE_CAP` (16) frames on surfaces a
  stream, the playhead's window one fewer; the 100 000-case proptest
  holds the cache to its model. Counted under the lock on every pass,
  those frames starved the presenter in `av_sync`, so the count is kept
  as frames come and go.
- **cli and doctor**: `frame --zero-copy` (exit 3, `import_failed`,
  when the GPU, its driver or the decoder can't), the JSON's
  `decode.transfer`, and doctor's `vulkan_devices[].dmabuf_import`
  (`docs/cli-json.md`). The overlay counts frames imported, copied and
  refused.
- **Tests**: Tier 1 has the plan's refusals, the uploader's imported
  path with planes made on lavapipe, byte for byte against the copy
  path, the cache's cap, and frame's refusals. lavapipe lacks
  `VK_EXT_image_drm_format_modifier`, so the import itself is Tier 2:
  `zero_copy_matches_copy_path` (48 frames each of HEVC Main and Main
  10, and an X5 lens, each the same bytes through its import, rendered
  15 frames after it came, as through its download),
  `zero_copy_frame_matches_the_copy` (frame) and
  `bench_x5_8k30_zero_copy_vs_copy` (each X5 mode on VA-API copied and
  imported, and on Vulkan Video, with the upload stages' p50 and p99).
  On odin (PR #9, at f3e860d) all three passed: every frame byte for
  byte through its import, an 8K perf clip's frame the same PNG both
  ways, and no import refused. Imported, VA-API decodes the X5's 1920²
  and 2880² modes about twice as fast as copied, level with Vulkan Video
  copied, its upload's CPU time a third or less, and the 8K modes three
  times and half again as fast; 3840² @ 25 dropped 25 frames imported,
  none copied (`docs/perf.md`). D25's choice before M3 takes these rows
  from odin and huginn.
- Path B, FFmpeg's Vulkan Video on wgpu's own device, follows on a
  branch of its own.

## Slice 5b: zero copy, path B

Built on `m1/zero-copy-b` (D25, D28, `docs/design/m1-5b.md`): FFmpeg's
Vulkan Video decodes on wgpu's own device, and each frame is copied on
the GPU out of FFmpeg's image into a buffer and from there into the
slot, with nothing through the CPU. Off by default, as path A is:
`SPHERECUT_ZERO_COPY=1` opens the viewer's device shared, and
`spherecut frame --zero-copy --decoder vulkan` renders one frame so.

- **render**: `render::vkshare`, render's second `unsafe` module.
  `Gpu::headless_shared` and `for_window_shared` open the device through
  wgpu-hal's `open_with_callback` with the video extensions,
  synchronization2 and the queues the plan picks (a decode queue and two
  compute queues wgpu never uses), or say why the device can't be
  shared (below Vulkan 1.3, no decode family, no second compute queue).
  `Copier` copies a frame's planes under FFmpeg's frame lock, waiting
  for and signalling its timeline semaphore, and `order_before` makes
  wgpu's next submission wait for the copy. `Uploader::upload_from_buffer`
  and `StillRenderer::render_copied` fill a slot from the buffer.
- **media**: `HwDevice::vulkan_shared` makes FFmpeg's context from the
  raw handles (`qf[]` from 7.1, the old fields on 6.1, the 9.0 fields
  behind `cfg(ffmpeg_9_0)`). `HwOutput::Vulkan` leaves frames on the
  device (`Transfer::Vulkan`), and `VideoFrame::vulkan()` gives core's
  `VkPicture`; a frame cropped on the left or top, or in a format the
  copy path converts, is downloaded, and its transfer says why.
- **playback, app, frame**: with zero copy on, a Vulkan Video stream
  decodes through FFmpeg's context on the viewer's device, made by the
  first open that wants it; the upload thread copies each frame out,
  lets it go and holds the buffer until the GPU has read it. A refusal
  turns zero copy off for the opens after, as path A's does. The
  overlay counts `vulkan` copies beside imports. frame's JSON says
  `"transfer": "vulkan"`, and it exits with `import_failed` where the
  device can't be shared, as on lavapipe.
- **Tests**: lavapipe has one queue and no decode, so Tier 1 checks the
  queue plans against RADV's, ANV's and lavapipe's families, the
  refusals, `upload_from_buffer` against the copy path for every
  layout, and, on a test-only device whose one queue FFmpeg, the copier
  and wgpu share, NV12 and P010 frames FFmpeg uploads into its images,
  copied out and into slots byte for byte as their bytes fill them, then
  downloaded by FFmpeg unchanged. Under the Khronos validation layer it
  reports none of the copier's or wgpu's work, only FFmpeg's own frame
  barriers (16, on 6.1 and 8.1 alike). Tier 2 has
  `shared_vulkan_matches_copy_path` (the clips of path A's check on
  Vulkan Video, 48 frames each, rendered through the copier 15 frames
  after they came, as through their download),
  `shared_vulkan_frame_matches_the_copy` (frame) and Vulkan Video's
  zero-copy rows in `bench_x5_8k30_zero_copy_vs_copy`; they run on odin
  next.
- doctor doesn't yet say whether a device can be shared: the log and
  frame's `import_failed` give the reason.

## Known issues and limits

- The GUI under winit 0.30.13 (D36): no touchpad pinch on Wayland or
  X11 (two-finger scroll and Ctrl+scroll zoom), no file drop on Wayland
  (the command line and the Files pane open files), and on GNOME sctk's
  plain window frame. No screen reader in M1 (no accesskit). egui's
  fonts have no emoji or arrows, so the buttons are words. It has run
  here only under kittest and Xvfb, with no compositor; H2 runs it on
  GNOME. A switch started during another jumps once to the nearer
  projection (D33). A worker of the last file that checked its open was
  current just before the next open can send one event after it, which
  lands on the next file's notices (D36).

- Spherecut opens only MP4/MOV and Matroska/WebM files; anything else,
  WAV, FLAC or MP3 music beds included, needs a DECISIONS entry that
  widens the demuxer list (D24). Within those containers FFmpeg may run
  any decoder it has (no `codec_whitelist`), each limited to 2^27 pixels
  per picture, which doesn't bound how many pictures a decoder keeps. A
  stream over the limit is a probe warning and a `VideoDecoder` error
  that give its size, except one whose frames say more than its
  container and whose size FFmpeg takes from the container (MJPEG, for
  one): that fails while decoding, as FFmpeg decides.
- The backend's hardware path has run here only on lavapipe, which
  decodes nothing, and on a software stand-in for the device (media's
  `fault-injection`). Its Tier 2 tests are written and skip here; they
  pass on odin's RX 7600, and huginn hasn't run them.
- A fallback to software is said by a `tracing::warn!`, the `FellBack`
  event and each frame's `DecodePath`, in the GUI by a warning notice
  (slice 7), and in `spherecut frame` by its result's `decode.fallback`
  and its text line, not as one of its `warnings` (D39). The overlay's
  decoder line and badge come with slice 8's overlay (D30).
- `spherecut frame --decoder auto` tries VA-API and never Vulkan Video
  (D39); elsewhere a caller asks for a kind. The player picks it from
  doctor's checks (D34, m1-plan amendment 9); `frame`'s `auto` keeps
  VA-API, since doctor's checks in each run would cost more than the
  frame (D39).
- Out of GPU memory, a slot
  is dropped; lavapipe can't run out, so only the ring's own test covers
  it, not a player's. The 5-second budget recheck has no test of its
  timer, only of what it sets (a set size shrinks when memory is short)
  and of `set_config`'s new budget reaching the cache. The present latency is one UI frame, not vsync's, until
  the GUI (slice 7). The GUI must make its `FfmpegBackend` on the
  rendering GPU's node (`FfmpegBackend::on_node`), so that decoding runs
  where doctor's checks ran. The cache's budget counts the frames it
  holds; the frame a worker waits to insert and frames evicted while
  still held are alive beside them, about a frame a stream more (88 MB
  with the X5's two 8K streams). With fewer than two GPU slots a
  stream live, the streams' frames after a seek go up one at a time.
  With no more live slots than streams (two of four Dead, on two
  streams), a seek or a step never lands, since every live slot holds
  a frame on screen. Slots die only when the GPU is out of memory, so
  the viewer is to treat a `SlotDropped` that leaves that few as a GPU
  failure (D34).
- Only `spherecut frame` asks wgpu's adapter for its GPU's render node
  (D30's lookup by bus id, then PCI ids). doctor has no adapter yet, so
  it tests `/dev/dri/renderD128` unless `--render-node` names another; on
  a machine with two GPUs the two can differ.
- `spherecut frame` here: every run renders on lavapipe or the CPU
  reference and decodes in software (`auto` with no render node), so
  `hw_unsupported` and `hw_failed` as errors, `auto`'s fallback after a
  device refused or failed, and `anisotropy_unavailable` (lavapipe has
  anisotropic filtering) have no Tier 1 test; the Tier 2
  `vaapi_render_matches_software` and `vulkan_render_matches_software`
  are written and skip here, and pass on odin since PR #6. They hold the
  hardware decode to software, and `auto` to VA-API on the node found
  from the rendering adapter's ids (D39), but they expect the device to
  decode, so `auto`'s fallback after a device refusal or failure and
  `anisotropy_unavailable` have no test of frame's in any tier yet.
  lavapipe's `--filter aniso` renders byte for byte what `iso` does
  (Mesa 25.2.8, on views that squeeze the source), so there the
  result's `"aniso"` changes nothing and nothing tells the two apart
  (D27, Next). The first Ctrl+C stops a seek or a read at once, but
  probe, the GPU's set-up, the render and the PNG's encode take no stop
  token, so during them it takes effect when the step ends, seconds
  later at a large `--size`. A frame started with SIGINT ignored keeps
  it ignored. The race test
  stages the file that appears at the output with a debug-only switch,
  `SPHERECUT_DEBUG_FRAME_RACE`; `persist_new`'s copy fallback is tested
  through a hook that refuses the link, since no filesystem here lacks
  hard links. A second Ctrl+C kills the run at once and may leave its
  `.spherecut-….tmp` beside the output. `-o /dev/null` and other
  non-files are refused (D16). The Euler table's pole rows can't be
  white and black at the centre pixel: there isotropic filtering at the
  analytic LOD blends the marker disc with the grid to grey (184 and 72
  here, as the f64 reference gives), so they assert a neutral centre and
  `the_markers_round_a_fisheye_turn_with_the_camera` checks the markers
  round the poles (D32). `--frame N` on a variable-rate stream decodes
  every frame from the first, so it slows the further in N is.
  `sigint_during_a_seek_cancels_with_exit_4` doubles `long-gop` to 1200
  frames when a 600-frame seek takes under 600 ms, which costs it about
  10 s more; here that is common, not the exception (the seek took
  0.6 to 0.8 s in slice 4's review, and the test 7.2 to 19.5 s).
- Matroska/WebM: frame refuses every Matroska file as `not_equirect`
  unless `--equirect` is given, since Spherecut doesn't read Matroska's
  spherical metadata (Spherical Video V2's `Projection` element) yet;
  probe doesn't report it either. A cut is found from the Segment's
  size (D13), so a Matroska file whose Segment's size is unknown (a live
  recording) that is cut isn't seen as cut: a `--time` past its end
  renders the last frame, as for an uncut file; `--frame N` there is
  checked against the constant rate and refused. Near a cut, frame
  also refuses a few frames the file still holds: frames the decoder
  drains after the failed read may follow frames the cut lost, so a
  seek whose next frame is one of them is `truncated` (D29).
- `vaapi_download_pool_reuses_buffers` and its Vulkan twin check the
  pool by the buffers it allocates, none after the warm-up, and report
  the decoding thread's minor faults per frame without a limit: on
  odin, VA-API's driver took 116 per 3840x1920 NV12 frame and 227 per
  P010 frame mapping each surface to read it, and at that size the
  faults can't show a pool that doesn't work, since glibc reuses freed
  frames' memory (D30, m1-plan amendment 12). FFmpeg's VA-API download
  creates, fills and destroys a frame-sized VAImage per frame, a second
  copy and, above 32 MiB (5760x2880 P010, 8K NV12), a fresh mapping per
  frame; the tests now report a 5760x2880 P010 clip to measure it, and
  slice 5 checks mapping the surface directly instead (D30).
- On lavapipe with FFmpeg 6.1, H.264 and HEVC are refused as `Withdrawn`
  (FFmpeg has Vulkan hwaccels for them, which can't start without a video
  queue), not `NotOffered`; only a codec without a Vulkan hwaccel, such
  as MPEG-4 Part 2, is `NotOffered` (D30).
- A device failure on a damaged packet is `HwFailed`, so the damage shows
  only once software decodes the packet after the fallback. A mid-stream
  fallback reopens the file and decodes from a keyframe.
- Damage is said once from one seek to the next by what tells it apart
  (D29): a packet with neither a byte position nor a timestamp, or a
  run of frames referring to a lost picture without a pts, is said each
  time decoding meets it. A trimmed HEVC stream that starts at a picture
  that isn't IRAP still decodes its start on a decoder opened afresh.
- In damaged H.264, a picture FFmpeg starts from a damaged slice header
  and then fails comes out with whatever its buffer held, and the frames
  that refer to it, up to the next IDR picture, depend on what the
  decoder decoded before: an exact seek there can return frames unlike
  the decode from the start's as `Exact`, and, unless another packet
  fails, with no `Damaged` event. Pre-existing, left for a decision on
  what such a seek should return (D29).
- `vaapi_seek_matches_software` and its Vulkan twin decode every sample
  stream from start to end on the device and make 50 exact seeks in each
  (20 above 2880², such as the X5's 8K takes) in software and on the
  device: an estimated 5 to 10 minutes each on huginn or odin for Pat's
  X5 set, most of it the software seeks at 8K. The noisy
  5.7K clips they use are 2 s, made the first time by the tests at about
  200 Mbps and cached in the target's `tmp`; hw-test's 20 s clips came
  with slice 5 (D37's perf clips). Their four cropped clips, two of them cropped on the left
  through FFmpeg's `h264_metadata` and `hevc_metadata` filters, are the
  only streams with a left crop: none of the X5 samples has one.
- Frames of a stream cropped on the left are now cropped exactly in
  software too, where slice 2a kept FFmpeg's aligned cropping, so their
  planes may start off a 32-byte boundary; swscale, converting one,
  warns once that this is slower (D30).
- Exact seeks in a stream whose timestamps go backwards decode it twice
  from the start, and such a stream is found out only once its
  timestamps are seen going backwards. The path for frames without
  timestamps can't be reached through MP4 or Matroska, so it has no
  test. A target among the leading pictures of the next keyframe seeks
  rather than decoding on. A Matroska file's constant rate is checked on
  its first 64 blocks and its Cues only. The new run that catching up
  starts after it skipped frames is a guard no test reaches. Resuming in
  a backwards stream after a frame handed out before the stream was found
  out takes the first frame at its pts for it, and starts a run (D29).
- The renderer and its goldens are calibrated on lavapipe (Mesa
  25.2.8, llvmpipe with LLVM 20.1.2): their PSNR floors are that
  driver's, each the measured PSNR less 3 dB (less 1 dB against the
  ideal), and another Mesa may need them recalibrated. lavapipe
  truncates an f32 stored into an f16 target, so the shaders round every
  f16 store to the nearest half themselves (D31). The anisotropic filter
  is built and validated but not compared with anything; the
  implicit-derivative debug mode is the goldens' seam and pole control.
  The render Tier 2 tests have run on one GPU, odin's RX 7600: at
  918d031 (PR #6) its CPU checks are the emulation's numbers, 8.57 dB or
  more above the floors, and huginn's 8060S hasn't run them (D32).
  The staging belt stays off: at H1 on odin it was never cheaper than
  `write_texture`, and the CPU trigger now asks it to be (D31); huginn's
  numbers can still turn it on. A lost device makes
  every render entry point return `gpu_failed`, and the engine then
  stops its uploads and sends `GpuFailed` (D34); recovering from it (a
  new device, the slots uploaded again) is the viewer's, in slice 7.
  16-bit sources
  lose their low 4 bits in the f16 planes, and a 360° frame whose width
  isn't a multiple of its chroma subsampling (odd-width 4:2:0, which AV1
  and VP9 allow) shows a half-texel chroma step at the ±180° seam (D31).
- The stored goldens match exactly only at the Mesa build they were
  blessed on. After a Mesa update here they are held by the other-driver
  rule (PSNR ≥ 48 dB, SSIM ≥ 0.995, at most 4 pixels more than 4 LSB
  off and none more than 8) until blessed again,
  and a blessing writes its day and commit into the manifest, so blessing
  again changes the manifest though no image does. lavapipe filters 8-bit
  planes to whole codes, so `range_full_vs_limited` and
  `siting_left_center` assert the plan's 2 and 1 LSB at 10 bits and only
  record NV12's 4 and 1.99. The ideal comparisons rest on the pixels 2.5
  levels down or more (2600 of the tiny planet's 65536, and 10044 and
  28532 in the shimmer rows); milder minification is compared with the
  CPU reference only. `shimmer_fisheye360`'s control has the least room:
  level 0 changes 3.26 times as much as the ideal, against a bound of 3
  (D32). A blessing holds each floor to the one blessed before it, within
  0.5 dB, not to the first, so blessings that each lower a floor a
  little can walk it down; the manifest's diff shows each step. The CPU
  comparison shares render's colour fold, so the colour rows' other
  truths and `color.rs`'s unit tests are what check the fold itself.
- Slice 4 adds about 28 s to a warm `cargo xtask check` here, the sum
  of what its tests take (76.4 s for the whole check at ff97714, before
  its review, without cargo-deny and cargo-audit). Its two new test
  binaries take 13.8 s (`frame.rs`, of which
  `a_frame_lost_to_a_cut_is_never_shown_in_its_place` is 7.2 s alone,
  most of it encoding its 150-frame clip, `every_failure_leaves_nothing_behind`
  4.9 s and `pipeline_goldens` 4.2 s) and 2.6 s (`tier2_frame.rs`'s
  Tier 1 stand-in), and `contract.rs` goes from 1.4 s without its frame
  tests to 12 to 17 s with them, nearly all of it
  `sigint_during_a_seek_cancels_with_exit_4`, which doubles `long-gop`
  more often than not here (above): it took 7.2 s when it didn't and
  15.0 to 19.5 s when it did in the review, and the binary 12.6 s in
  the last run; `an_ignored_sigint_stays_ignored` runs beside it. The
  strace test for frame and core's new unit tests run beside the others
  in their binaries and add under 0.2 s. The slices' numbers below were
  measured on other days, and this box's timings vary by up to a third
  between runs (render's `goldens.rs` took 10.5 s in this check against
  7.2 to 7.4 s in slice 3b's).
- Slice 3b adds about 8 s to a warm `cargo xtask check` (42.2 s against
  34.3 s at 09342c6, both without cargo-deny and cargo-audit): the
  goldens' test binary takes 7.0 s on lavapipe, `goldens` itself 6.9 s
  of it, and its two shimmer rows 4.5 s of that (each renders 16 views
  and supersamples 8 ideal ones at 16×16 a pixel). Building render and
  testkit at opt-level 2 in dev profiles keeps it there: at 0 the
  goldens took 16.9 s. The adapters test and the bench's smoke run take
  under a second. The fixes after its review add about 2 s more (41.9 s
  against 39.6 s with 642ccb4's render and synth, run alternately on the
  same day, two runs each): the goldens' binary takes 7.2 to 7.4 s
  against 6.8 to 6.9, since the harness's own tests now bless their rows
  into scratch directories, and the upload tests 1.7 s against 0.8, for
  the bench's block test on a pair of 3840² frames and the 16-bit ingest.
- Slice 3a adds about 2.5 s to a warm `cargo xtask check` (33.6 s
  against 31.1 s at ef1a9d1, both without cargo-deny and cargo-audit,
  which here depend on the advisory database; 33.2 s before its first
  review's fixes): the render tests' four binaries take 2.4 s on
  lavapipe (1.8 s before the review's tests), and the new sphere and
  testkit tests the rest.
  A cold check also builds wgpu and its tree once, about a minute here.
- Slice 2a adds about 12 s to `cargo xtask check`, nearly all of it the
  seek oracle's test binary: 11.7 s of a 26 s warm check here (8.9 s
  before the second review's fixes added the 25-frame open-GOP, RADL,
  spliced and resumed clips, 4.6 s before the first review's added the
  tied, backwards, variable-rate Matroska, long Matroska and catch-up
  ones). Every exact seek in a backwards clip decodes it twice from the
  start, and the oracle seeks four times in each frame's interval. Slice
  2b adds about 5 s more: the fallback tests (3.9 s here), which fail
  every frame of two clips, and the Tier 1 checks of the Tier 2 checks.
- FFmpeg 6.1's H.264 decoder, left to drop the frames it decodes from
  references a seek skipped, loses the frames still in its reorder buffer
  at the end of the stream, and doesn't flag those it drains then.
  Spherecut has it output them, drops the flagged ones and, after a seek,
  every leading picture of the keyframe it landed on, by pts (D29);
  FFmpeg 7.1 to 9.0 have not been checked for the bug.
- swscale's conversion is tested end to end only for UYVY and RGB24, the
  formats a raw MOV carries; the others by format (D30).
- x265 (3.5 here, and its master) leaves the dts of the frames within
  its B-frame delay undefined when a clip ends inside that delay: one
  frame, or two with a B-pyramid, gets its pts minus whatever x265's
  heap held. FFmpeg 6.1's mov muxer writes it; 8.1.3's refused it in
  about half of the runs of `pipeline_goldens` against 8.1.3 in slice
  4's review ("pts/dts pair unsupported", `AVERROR_PATCHWELCOME`), in
  its one-frame lossless clips. synth now writes HEVC clips of at most
  `X265_BFRAME_DELAY` (2) frames, and frame's tests their one-picture
  clip, without B-frames (`writers.rs`
  `hevc_clips_within_x265s_bframe_delay_have_no_b_frames`); the
  pictures, and so the goldens, are the same. gen-media's
  `hifreq-hevc-lossless` and `seam-hevc-lossless` (two frames) change
  bytes but not pixels.
- synth's clips are tagged centre-sited chroma, which is what their
  4:2:0 is, so probe's JSON says `center` for them, `gen-media`'s set
  included.
- `VideoDecoder` copies hardware frames to system memory. Zero-copy
  (VA-API to DMA-BUF to wgpu) is proved in M1, and whether it becomes the
  default is decided before M3 (D25).
- `VideoDecoder` finds a Matroska cut by the Segment's size, as `probe`
  does, but not a cut in a Matroska file whose Segment's size is unknown
  (a live recording), nor a fragmented MP4 cut between fragments, which
  reads as a shorter file (D13, D15).
- Without decoding, `probe` can't find damage inside `mdat`, or damage in
  `moov` that keeps the structure consistent, such as changed sample
  sizes or times (`stts`, `ctts`, `elst`), chunk offsets that still point
  into `mdat`, or changed bits of a parameter set. Bytes of `mdat` that no
  chunk uses are deliberately not damage. Of 55 generated files corrupted
  the way a9128cf made `bad-corrupt-moov.mp4` (64 byte flips in the second
  half), 16 still pass `probe` at 8435b8b, as at 9d212c3. Their reports
  match the clean file's, apart from FFmpeg's decode errors in two, one
  byte of a handler name in one, and in one a display matrix that rotates
  by 0 degrees (D15).
- The decoder's invalid-data skip path has no test.
- Some tests and the encode paths still call ffmpeg-next accessors that
  build C enums (D11).
- `inject` and `Mp4Writer` replace only a regular file, so an output that
  is a directory, FIFO, device or socket is refused: `-o /dev/null` and
  writing to a pipe aren't supported, and would need a non-atomic path of
  their own (D16). They check the output before any work and again just
  before the rename, but no rename call replaces only a regular file, so
  a pipe or device created at the output in the microseconds between the
  last check and the rename would still be replaced.
- `inject` refuses damaged input: anything `Mp4Tree::damage()` finds,
  which includes the box reader's warnings. When `moov` comes before
  `mdat`, it also refuses edits that take `moov` past the box reader's
  limit (`MAX_BOXES`, 250 000 boxes), found when it re-reads `moov` to
  shift the chunk offsets. With `moov` at the end of the file no offsets
  move and there is no re-read, so such edits give a valid MP4 that
  Spherecut's own box reader stops partway through, and `probe` reports
  as damaged. No chunk offset is ever wrong.
- `hw-test` runs the `spherecut doctor` it built through a lossy UTF-8
  copy of the binary's path, so a cargo target directory whose path isn't
  UTF-8 makes that run fail; the report shows the failure.
- The Insta360 trailer is detected but not parsed, so `probe` shows no
  model or firmware.
- GoPro and DJI detection comes from format research only; no samples
  have been checked.
- Fedora's `ffmpeg-free` lacks libx264 and libx265, which `gen-media` and
  the Tier 1 media tests need. Use RPM Fusion's `ffmpeg`. Tier 1 also needs
  libdav1d, an FFmpeg built with Vulkan, Mesa's lavapipe and strace.
  Against a 9.0.2 built without Vulkan, every Tier 1 test passes except
  those that need FFmpeg's Vulkan device: `crates/cli/tests/doctor.rs`
  and `crates/media/tests/hw_refusal.rs` (D2).
- `cargo xtask check` builds only against the FFmpeg pkg-config finds,
  unless `SPHERECUT_FFMPEG_COMPAT` lists other prefixes, and a fresh
  checkout has none: other builds are checked only where someone has
  built them (`scripts/build-ffmpeg.sh`) and set the variable. Here the
  check ran without it, and `cargo xtask compat` was run against 7.1,
  7.1.3, 8.1.3 and 9.0.2 by hand (D2). compat links the tests against each FFmpeg but runs them against one only, so
  a behaviour that differs between versions is found only on that one.
- An exact seek in H.264 whose sync samples FFmpeg doesn't flag
  keyframes as frames (I-frames that aren't IDR, with no recovery point
  SEI or one that counts frames, and x264's intra-refresh recovery
  points on P-frames) decodes from the start of the stream (D29):
  nothing in the file says the frames after them don't refer across
  them. It is right, and slow in a long file; keyframe seeks aren't
  affected. The X5's LRV proxies aren't such streams: their keyframes
  are IDR pictures, and their exact seeks cost what they did.
- On FFmpeg 7.1.x, HEVC damage that the decoder skips (a slice header or
  frame start it can't decode) is silent: no error, no frame flagged
  corrupt, so no `Damaged` event, where 6.1 and 8.0 and later raise one
  (D29). Fedora 43 ships 7.1.
- `VideoDecoder::open`, the M0 decoder the media tests and Tier 2 checks
  use, doesn't ask FFmpeg for corrupt frames
  (`AV_CODEC_FLAG_OUTPUT_CORRUPT`), and without that flag FFmpeg 8.0 and
  later refuses the HEVC pictures that refer to a damaged reference
  picture, skipping them with "Skipping invalid undecodable NALU" up to
  the next IRAP picture: there the damage loses those frames without an
  error. The backend's decoder, which everything above media uses, asks
  for them and reports it (D13, D29).
- With every seek oracle test opened on one thread, as hardware decoders
  run, all of them pass on 6.1 and on 8.1.3 but
  `an_edit_list_hides_its_lead_in_and_clip_time_starts_after_it`, whose
  cost check fails on both: an exact seek to frame 10 of `edit.mp4`,
  whose keyframe is at or before the target, seeks (once, decoding 7
  frames) where the check expects it to decode forward. The frames it
  returned until then were right. Tier 1 runs the oracle on frame
  threads, and on one thread only
  `on_one_thread_no_frame_from_before_a_seek_comes_out_after_it` (D29).
- `hw-test`'s prerequisite check still judges Fedora by installed packages
  (`ffmpeg-free`, `mesa-va-drivers-freeworld`), unlike doctor (D18).
- `hw-test`'s `vainfo` and prerequisite checks look only at
  `/dev/dri/renderD128`.
- Doctor's Vulkan checks run on FFmpeg's default Vulkan device, while the
  Tier 2 Vulkan Video tests use the GPU behind VA-API's render node
  (`open_vulkan_device`). On a machine with two GPUs they can differ;
  both name the device (D38).
- Doctor doesn't print the full VA-API profile list; the hw-test report
  includes `vainfo`.
- ambiX AAC plays as 4.0 surround in players that ignore `SA3D` (D17).
- On huginn, VA-API decode of the X5 8K clips varies a lot between runs
  (1.2 to 2.9 times real time; `docs/perf.md`), which hw-test marks as
  unstable. The copy of each frame to RAM is a likely part of it; the M1
  performance overlay will show how much.
- On odin, VA-API with its copy to RAM is below real time at X5 5.7K60 and
  8K30 and slower than software on the synthetic clips, while Vulkan Video
  keeps up; huginn's unified memory doesn't show this (`docs/perf.md`).
  Slice 5b measures zero-copy for both paths (m1-plan amendment 9).
- huginn has no Vulkan Video H.264 or HEVC until it gets RPM Fusion's
  `mesa-vulkan-drivers-freeworld`, as odin has (Pat's call).
- No Intel machine until the Framework arrives.
- No GUI yet, so no GUI smoke run (M1).
- The JSON contract's doctor error path (an issue of severity error,
  `doctor_errors`, exit 1) is tested on a constructed report, and the
  binary only for `ok` matching its exit status: this workspace's FFmpeg
  has every decoder doctor requires. doctor's issues have no codes of
  their own, and the envelope's `warnings` is always empty for probe and
  doctor; their findings are in `result` (D27).
- `spherecut probe` with no file now exits 2 (a usage error) where M0
  exited 1, and probe's `--json` shape changed from M0's (an object for
  one file, an array for several) to the envelope; `docs/cli-json.md` has
  the new one.
- In the contract, probe checks a Matroska file only for its length, so
  one damaged inside that FFmpeg can't open is `open_failed`, with
  FFmpeg's log, rather than `damaged`. An MP4's `full` check covers
  `moov`'s sample tables but not a fragmented file's `moof`/`trun`, so
  fragments whose samples lie past the end of the file pass unnoticed. A panic under `--json` is an
  `internal` document (exit 1); a crash by signal, such as a fault
  inside FFmpeg, still leaves stdout empty. The unreadable-file test
  runs the binary as nobody through `setpriv` when the tests run as
  root, with a readable copy beside the file to show that nobody reaches
  the directory; only probe's unit test covers it where neither works,
  or where nobody can't reach the target directory.
- The playback benchmarks have run on one GPU, odin's RX 7600 (H1,
  "odin's fifth run"); here they skip (no GPU), and only their harness
  runs, in Tier 1 on lavapipe and in software (D34). Once in four runs
  at the fixes, the 10-bit gate's unthrottled pass slowed to 17 to 20
  fps for 3 s while the cache filled, its new frames' downloads taking
  50 to 56 ms; three later runs, one with free memory fragmented, didn't
  show it, and its cause isn't known. The X5 raw and 8K numbers are to
  be measured again after the fixes.
- In the benchmarks the X5's raw view is the two lenses projected side
  by side, 960×1080 each, through the same rectilinear projection: the
  GUI's raw view comes in slice 7, and its cost may differ.
- hw-test's first full run on a machine is long: libx265 took 27 minutes
  here, on 4 CPUs, for the 5.7K 8-bit perf clip alone. The clips are
  kept for later runs, and `--quick` makes none.
- The engine is in no binary until the GUI (slice 7), so
  `SPHERECUT_FRAME_CACHE_MIB`, which `playback::budget` reads, is not in
  the README yet.
- D37 is a draft: the GUI smoke run, its
  session discovery and screenshots, and their pass rules are slice 8's,
  for H2.
- Audio has never played on a sound card. This box has none, so the
  cpal stream, `audio-out`'s loop, its error handling and its retry are
  built but not run; the fake and null devices run the rest, the
  callback included. `audio_output_device_plays` runs in H2 (D35).
- Without a sound card, libasound prints its own errors to stderr
  ("ALSA lib conf.c: ... Evaluate error") when cpal's ALSA host looks
  for one, before Spherecut plays on the null output; a GUI started
  from a terminal will show them.
- A file whose AAC uses perceptual noise substitution decodes those
  bands differently after a seek than from the start, at their noise
  floor; synth's clips don't use it. A Matroska file's audio is placed
  within a millisecond, and may run up to one AAC frame longer than its
  MP4 (D35).
- The output's rate and channel count are fixed when a file opens: a new
  default device that can't play them leaves playback silent on the
  system clock until one can, or the file opens again. The latency is
  the host's report, not measured (D35).
- The engine's audio events (`AudioOpened`, `NoAudio`, `AudioLate`) and
  `Player::audio_stats()` have no reader until the GUI's notices and the
  overlay (slices 7 and 8); "ambisonic: basic stereo decode" is the Info
  pane's, in slice 7.

## Next

1. **Slice 5b on odin, path B, and huginn's H1.** odin's sixth run
   (PR #8) is clean but for the Tier 1 test now fixed, and slice 5b's
   path A (PR #9), the download pool's fix for VA-API's 8K slowdown and
   path B (PR #10) passed there. huginn runs `cargo xtask hw-test --samples
   <samples>` at main, without `--quick` so that it makes the perf clips,
   on the power profile Pat edits with. It hasn't run slice 3b's to 6's
   Tier 2 tests: the goldens hold its 8060S, RADV too, to lavapipe's
   floors and D32's Revisit clause to within 1 dB of the emulation's
   numbers; `vaapi_render_matches_software` and `--decoder auto` on the
   node found from the rendering adapter (D39) run there first; its
   upload numbers can still turn the belt on (D31); and its playback
   gates run on VA-API, since RADV on Strix Halo has no Vulkan Video
   HEVC. The GUI (slice 7) is in, on render's own device through
   egui-wgpu's `WgpuSetup::Existing` (D31), so path B's Vulkan Video
   device reaches the viewer. Slice 8 (the overlay, settings, `--smoke`
   and H2's GUI run) follows.
   Still with no test in any tier, for huginn's H1 or later:
   `auto`'s fallback after a device that turns a stream down or fails
   on a frame, `anisotropy_unavailable`, and whether `--filter aniso`
   does anything: a Tier 2 check on RADV or ANV that a grazing view
   with `aniso` differs from `iso`, or is closer to `render_view_ideal`,
   belongs with render's Tier 2 at H1 or H2, since lavapipe's `aniso`
   renders as `iso` does (D27). Reading
   Matroska's spherical metadata (Spherical Video V2's `Projection`
   element, which FFmpeg gives as side data) in probe, so that frame
   takes a tagged Matroska equirect without `--equirect`, waits for a
   slice that needs Matroska sources.
2. H.264 damage FFmpeg hands out from old buffers (D29): read
   FFmpeg's log per decoder (its callback names the codec context) for
   "decode_slice_header error" and the like, and treat what it says as
   damage, so an exact seek past it is `Truncated` and `spherecut frame`
   fails, as for damage FFmpeg reports; and frame's warning should say
   where the damage was, before or after the frame.
3. The Insta360 trailer reader (metadata, calibration, IMU), needed from M2.
4. Agent integration A1 (the MCP server) once M1's code is in, while
   M1's hardware report is pending (Pat chose this order on 2026-10-24).

## How to run

```sh
cargo xtask check                 # everything Tier 1 (add --fix to apply rustfmt)
cargo xtask gen-media             # synthetic media into test-media/
cargo test -p spherecut-synth --test seek   # the seek oracle (D29)
cargo test -p spherecut-synth --test fallback   # hardware fallback, with fault injection (D30)
cargo test -p spherecut-render    # the render core on lavapipe against its CPU reference (D31)
cargo test -p spherecut-render --test goldens   # the golden images (D32)
SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test goldens   # bless them, after a change meant to move them
SPHERECUT_UPDATE_GOLDENS=lower cargo test -p spherecut-render --test goldens   # bless, letting floors drop by more than 0.5 dB (D32)
cargo run -p spherecut-cli -- probe <file>... [--json]   # --json: one document, docs/cli-json.md
cargo run -p spherecut-cli -- doctor [--json]
cargo run -p spherecut-cli -- frame <file> -o out.png [--time 12.5 | --frame 370] [--yaw 30 --pitch -10 --roll 0] [--fov 90] [--json]   # README, "Rendering a frame"
cargo test -p spherecut-cli --test frame   # frame end to end, with the pipeline goldens (D32)
SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test frame pipeline_goldens   # bless the pipeline goldens
cargo xtask hw-test               # on a real machine; report in hw-reports/
cargo xtask hw-test --list        # the Tier 2 tests it runs, and stop
cargo test -p spherecut-playback --test tier2_playback   # the playback benchmarks' harness on lavapipe (Tier 1; hw-test runs the benchmarks, D34)
cargo test -p spherecut-synth --test audio   # audio decoding: exact samples, seeks, resampling, channels (D35)
cargo test -p spherecut-audio     # the chunk ring and the audio clock, on the fake and null devices (D35)
cargo test -p spherecut-playback --test av_sync   # A/V sync through the engine on the fake device (D35)
scripts/build-ffmpeg.sh n8.1.3 ~/ffmpeg   # an FFmpeg release from its tag into ~/ffmpeg/n8.1.3-full (about 4 min on 4 CPUs)
cargo xtask compat ~/ffmpeg/n7.1.3-full ~/ffmpeg/n8.1.3-full --test ~/ffmpeg/n8.1.3-full   # clippy and a link against each (about 3 min and 1.4 GB a prefix on 4 CPUs), Tier 1 against one (D2); targets are spherecut-compat-<label> in $SPHERECUT_FFMPEG_COMPAT_TARGET_DIR or target/compat/, and --clean deletes them after
SPHERECUT_FFMPEG_COMPAT=~/ffmpeg/n8.1.3-full cargo xtask check   # check lints and links against those too
```
