# Partial run during video playback (does not count)

The first `cargo xtask hw-test` at 0b3ab9b on odin, started 2026-11-11 16:59 PST. Pat was watching YouTube in Firefox while it ran, so its timings don't count.

- I stopped it at about 17:12, after the GUI tests and part way through `tier2_export`.
- xtask writes its report only at the end, so no report exists. The files here are what it left in `target/`: `hw-bench.tsv`, and the `hw-gui` JSONs and screenshots.
- `gui_smoke` was 5/5 OK and `gui_proto` 3/3 OK.
- `gui_decode_behind`:
  - run 0 showed 179 of 270 frames (66%) with no failure;
  - run 1 (VA-API copied) failed: "UI p99 49.3 ms, over 1.5 × the 60 Hz period (25.0 ms)", with 161 of 570 frames shown (28%);
  - the control (run 2) presented 33%.
