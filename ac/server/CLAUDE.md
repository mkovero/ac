# server/ — ZMQ measurement server

## Architecture

`engine.py` is the main loop: it binds a ZMQ REP socket (ctrl) and PUB socket (data), receives JSON commands from clients, dispatches to worker threads, and streams results back over the PUB socket.

Entry point: `ac server enable` (blocking) or auto-spawned by the client as a subprocess with `--local`.

## File responsibilities

| File | Purpose |
|------|---------|
| `engine.py` | ZMQ REP+PUB main loop, command dispatch, worker spawn |
| `audio.py` | `JackEngine` (JACK real-time I/O), `find_ports()`, `port_name()`, backend factory (`get_engine_class()`, `get_port_helpers()`) |
| `sd_audio.py` | `SoundDeviceEngine` (PortAudio fallback), matching duck-typed contract |
| `analysis.py` | `analyze(recording, sr, fundamental)` — FFT THD/THD+N |
| `jack_measure.py` | `jack_sweep_level/frequency/monitor` — legacy direct functions |
| `jack_calibration.py` | `Calibration` class + `run_calibration_jack_zmq()` |
| `signal.py` | `make_sine()` — sine buffer generator |
| `dmm.py` | SCPI socket client for Keysight 34461A DMM |

Shared modules (at `ac/` root, imported as `from ..constants import ...`):
`constants.py`, `conversions.py`, `config.py`

## ZMQ protocol

- **CTRL** `tcp://*:5556` REP — JSON command/reply
- **DATA** `tcp://*:5557` PUB — `b"<topic> <json>"` frames

Topics: `data`, `done`, `error`, `cal_prompt`, `cal_done`

Commands: `status`, `quit`, `stop`, `devices`, `setup`, `get_calibration`, `list_calibrations`, `sweep_level`, `sweep_frequency`, `monitor_thd`, `monitor_spectrum`, `generate`, `calibrate`, `cal_reply`, `dmm_read`

## Calibration model

`Calibration` stores `vrms_at_0dbfs_out` and `vrms_at_0dbfs_in` — scalar physical voltage at 0 dBFS full scale. No per-frequency correction; all commands (sweep, plot, monitor, transfer) use the same `_level_to_dbfs()` → `level_dbfs` → `amplitude` pipeline for stimulus level. Key format: `out{N}_in{M}`. Stored in `~/.config/ac/cal.json`.

## Result dict keys (from `analyze()`)

`fundamental_hz`, `fundamental_dbfs`, `linear_rms`, `thd_pct`, `thdn_pct`, `harmonic_levels`, `noise_floor_dbfs`, `spectrum`, `freqs`, `clipping`, `ac_coupled`

Sweep worker frames add: `drive_db`, `out_vrms`, `out_dbu`, `in_vrms`, `in_dbu`, `gain_db`, `vrms_at_0dbfs_out`, `vrms_at_0dbfs_in`

## Stale server detection

`_SRC_MTIME` is set at import time to the max mtime of all `.py` files in `server/`. The client compares this against its own scan; if the server is older, the client sends `quit` and respawns.

## Dependencies

`numpy`, `scipy`, `zmq`, `sounddevice`. Optional: `jack` (python-jack / CFFI binding to libjack) for JACK backend
