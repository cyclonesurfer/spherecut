# Logs of the counted run at 0b3ab9b

- `times.txt`: each step's exit code and wall time, with Firefox, audio streams and idle CPU at its start and end.
- `load.log`: uptime and top before each step.
- `idle-check.log` and `quiet.log`: the checks before the run.
- `gdctl-*.txt`: the display layout before the 125% step, during it, and after it was restored.
- `behind2-gui/` and `125-gui/`: the hw-gui JSONs of steps 3 and 5, copied before the next hw-test cleared them. Step 2's were cleared before they could be copied.
- `export8k.log`: `export_360_8k_vaapi` rerun alone afterwards (25 s, pass), for its printed line.
