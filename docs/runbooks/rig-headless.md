### Headless rig runbook

`ac transfer` launches `ac-view`, and `ac plot ir` runs the measurement and
then discards it (`run_ir` waits for `done` and prints nothing — the gap
epic #276 exists to close). Neither is usable over SSH, so a rig session
drives the daemon through two examples that build from the tree:

```bash
# transfer_stream, two pairs, 20 s, driven at -30 dBFS, frames to JSON Lines
cargo run --release -p ac-daemon --example transfer_probe -- \
  --pairs "2,2;0,2" --seconds 20 --drive-dbfs -30 --out run.jsonl

# plot_ir, reporting peak, floor, SNR, offset from window centre, and onset
cargo run --release -p ac-daemon --example ir_probe -- \
  --level-dbfs -30 --duration 2.0 --f1 50 --f2 16000 --window 16384
```

Both take `--ctrl-port` / `--data-port` for a daemon on non-default ports;
the `ac` CLI takes `AC_CTRL_PORT` / `AC_DATA_PORT` for the same purpose, and
`ac setup output <N> input <N>` retargets a **running** daemon so successive
measurements share one client (below).

`transfer_probe` starts the session `drivable`, so it comes up silent and
`set_drive` raises it, refreshing the 1500 ms dead-man every 250 ms. Without
`--drive-dbfs` the session is passive and opens no output port.
`--level-dbfs` is **mandatory** on `ir_probe`: `plot_ir` does not apply the
config's `drive_max_dbfs` ceiling — only `set_drive` does — so that value is
the only limit on what reaches the interface.