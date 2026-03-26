# client/ — CLI and ZMQ client

## Architecture

`parse.py` → `ac.py` (AcClient + handlers) → ZMQ server

`parse.py` classifies positional tokens by unit suffix and returns a plain dict. `ac.py` creates an `AcClient`, ensures the server is running, and dispatches to a handler function.

## File responsibilities

| File | Purpose |
|------|---------|
| `ac.py` | `AcClient` (ZMQ REQ+SUB), dispatch table, `cmd_*` handlers, `main()` |
| `parse.py` | Token-based CLI parser, `ABBREVS`, `USAGE` string |
| `tui.py` | `SpectrumRenderer` — terminal bar chart with peak hold |
| `io.py` | `save_csv()`, `print_summary()` |

Shared modules (imported as `from ..x import ...`):
`constants.py`, `conversions.py`, `config.py`

Cross-subpackage imports:
- `from ..server.jack_calibration import Calibration` (used in `_cal_from_frame`)
- `from ..server.jack_calibration import DEFAULT_CAL_PATH` (used in calibrate_show)
- `from ..server.engine import run_server` (used in server_enable handler)

## CLI grammar

```
ac sweep level   <start:level> <stop:level> <freq:freq> [<step:step>]
ac sweep freq    <start:freq>  <stop:freq>  <level:level> [<ppd:ppd>]
ac monitor       [<start:freq> <end:freq>] [<interval:time>]
ac generate sine [<channels>] <level> [<freq>]
ac calibrate     [output N] [input N] [<level>]
```

Token suffixes: `hz`/`khz`, `dbu`/`dbfs`/`vrms`/`mvrms`/`vpp`, `db`, `ppd`, `s`

## AcClient ZMQ protocol

- REQ socket → CTRL port 5556: `send_cmd(dict)` → reply dict
- SUB socket → DATA port 5557: `recv_data()` → `(topic, frame)`

After a recv timeout the REQ socket must be recreated (`_reconnect_ctrl`).

## Auto-spawn behavior

`_ensure_server()` checks `status` response. On localhost: auto-starts with `--local` flag if not responding; restarts if `src_mtime` is stale (server's max .py mtime < client's scan of `server/`).

## TUI smoothing constants (SpectrumRenderer)

`_FALL_ALPHA = 0.20`, `_PEAK_HOLD = 6` frames, `_PEAK_DECAY = 1.5 dB/frame`

## Dependencies

`numpy`, `zmq`, `pyqtgraph` (for GUI views)
