# Partial run under Firefox load (does not count)

The second `cargo xtask hw-test` at 0b3ab9b on odin, started 2026-11-11 17:40 PST.

Before it started I checked from 17:09 to 17:39 PST. Firefox played no audio (both its streams were corked), but its processes together used 40–135% CPU (see `../odin-0b3ab9b-run/quiet.log`).

- Pat then closed Firefox, and I stopped this run at 17:45, during Tier 2.
- No report was written. The files here are what it left in `target/`.
