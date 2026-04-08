# ZMQ Wire Protocol

This document is the authoritative reference for the JSON protocol spoken
between any `ac` server (Python or Rust) and any `ac` client.

---

## Transport

| Socket | Bind address (local) | Bind address (public) | Type | Direction |
|--------|---------------------|-----------------------|------|-----------|
| CTRL   | `tcp://127.0.0.1:5556` | `tcp://*:5556` | REP | client → server (request/reply) |
| DATA   | `tcp://127.0.0.1:5557` | `tcp://*:5557` | PUB | server → client (push only) |

**CTRL** is a strict REQ/REP pair: the client sends one JSON object, waits
for one JSON object reply. No pipelining.

**DATA** frames are UTF-8 strings with a topic prefix:

```
<topic> <json-object>\n
```

e.g. `data {"type":"sweep_point", ...}` or `done {"cmd":"plot", ...}`.

Topics: `data`, `done`, `error`, `cal_prompt`, `cal_done`, `gpio`.

---

## CTRL reply envelope

Every CTRL reply contains at minimum:

```json
{ "ok": true | false }
```

On failure: `"ok": false, "error": "<human-readable string>"`.

---

## DATA frame envelope

Every DATA frame published on the PUB socket is prefixed with a topic word.
The JSON payload always includes `"cmd"` (which command produced it) and
`"type"` (the frame subtype) where applicable.

**Terminal topics** — a client waiting for a measurement to finish should
stop consuming frames when it receives either of these:

| Topic    | Meaning |
|----------|---------|
| `done`   | command completed successfully |
| `error`  | command failed; contains `"message"` field |

---

## Shared types

### `sweep_point` frame

Emitted by `plot` and `plot_level` for each measured frequency or level point.

```json
{
  "type":             "sweep_point",
  "cmd":              "plot" | "plot_level",
  "n":                <int>,          // 0-based sequence number
  "drive_db":         <float>,        // stimulus level in dBFS
  "freq_hz":          <float>,        // present for plot_level; absent for plot (freq is the sweep axis)
  "thd_pct":          <float>,
  "thdn_pct":         <float>,
  "fundamental_hz":   <float>,
  "fundamental_dbfs": <float>,
  "linear_rms":       <float>,        // 0–1 dBFS scale
  "harmonic_levels":  [[<hz>, <amp>], ...],  // 2nd, 3rd, … harmonics
  "noise_floor_dbfs": <float>,
  "spectrum":         [<float>, ...], // downsampled to ≤ 1000 points, DC bin removed
  "freqs":            [<float>, ...], // matching frequency axis (Hz)
  "clipping":         <bool>,
  "ac_coupled":       <bool>,
  "out_vrms":         <float> | null, // null when uncalibrated
  "out_dbu":          <float> | null,
  "in_vrms":          <float> | null,
  "in_dbu":           <float> | null,
  "gain_db":          <float> | null,
  "vrms_at_0dbfs_out":<float> | null,
  "vrms_at_0dbfs_in": <float> | null
}
```

### `spectrum` frame

Emitted continuously by `monitor_spectrum`.

```json
{
  "type":             "spectrum",
  "cmd":              "monitor_spectrum",
  "freq_hz":          <float>,        // auto-detected dominant frequency
  "sr":               <int>,          // sample rate (Hz)
  "freqs":            [<float>, ...], // downsampled, DC removed
  "spectrum":         [<float>, ...],
  "fundamental_dbfs": <float>,
  "thd_pct":          <float>,
  "thdn_pct":         <float>,
  "in_dbu":           <float> | null,
  "clipping":         <bool>,
  "xruns":            <int>
}
```

---

## Commands

---

### `status`

Returns server health and current state.

**Request**
```json
{ "cmd": "status" }
```

**Reply**
```json
{
  "ok":             true,
  "busy":           <bool>,
  "running_cmd":    "<name>" | null,
  "src_mtime":      <float>,          // max mtime of server source files
  "listen_mode":    "local" | "public",
  "server_enabled": <bool>
}
```

---

### `quit`

Requests the server process to exit cleanly after the current reply.

**Request**
```json
{ "cmd": "quit" }
```

**Reply**
```json
{ "ok": true }
```

---

### `stop`

Stops one or all running workers.

**Request**
```json
{ "cmd": "stop" }
{ "cmd": "stop", "name": "<worker-name>" }
```

`name` is optional. When omitted, all workers are stopped.

**Reply**
```json
{ "ok": true }
```

**DATA** — after stop, the worker emits a terminal frame:
```json
// topic: done
{ "cmd": "<worker-name>" }
```

---

### `devices`

Lists available JACK/PortAudio ports.

**Request**
```json
{ "cmd": "devices" }
```

**Reply**
```json
{
  "ok":                true,
  "playback":          ["<port-name>", ...],
  "capture":           ["<port-name>", ...],
  "output_channel":    <int>,
  "input_channel":     <int>,
  "output_port":       "<sticky-name>" | null,
  "input_port":        "<sticky-name>" | null,
  "reference_channel": <int> | null,
  "reference_port":    "<sticky-name>" | null
}
```

On error (e.g. JACK not running):
```json
{ "ok": false, "error": "<message>" }
```

---

### `setup`

Reads or updates persistent hardware config (`~/.config/ac/config.json`).

**Request** — read (no changes):
```json
{ "cmd": "setup", "update": {} }
```

**Request** — write:
```json
{
  "cmd":    "setup",
  "update": {
    "output_channel":    <int>,     // optional
    "input_channel":     <int>,     // optional
    "reference_channel": <int>,     // optional
    "dbu_ref_vrms":      <float>,   // optional
    "dmm_host":          "<host>" | null,  // optional
    "server_enabled":    <bool>,    // optional
    "backend":           "jack" | "sounddevice" | null  // optional
  }
}
```

When `output_channel`, `input_channel`, or `reference_channel` is updated,
the server resolves and stores the sticky port name automatically.

**Reply**
```json
{
  "ok":     true,
  "config": { /* full config dict, all keys */ }
}
```

---

### `get_calibration`

Look up a stored calibration entry.

**Request**
```json
{
  "cmd":            "get_calibration",
  "output_channel": <int>,   // optional, defaults to config value
  "input_channel":  <int>    // optional, defaults to config value
}
```

**Reply — found**
```json
{
  "ok":                true,
  "found":             true,
  "key":               "out0_in0",
  "vrms_at_0dbfs_out": <float> | null,
  "vrms_at_0dbfs_in":  <float> | null,
  "ref_dbfs":          <float>
}
```

**Reply — not found**
```json
{ "ok": true, "found": false }
```

---

### `list_calibrations`

Returns all stored calibration entries.

**Request**
```json
{ "cmd": "list_calibrations" }
```

**Reply**
```json
{
  "ok": true,
  "calibrations": [
    {
      "key":               "out0_in0",
      "vrms_at_0dbfs_out": <float> | null,
      "vrms_at_0dbfs_in":  <float> | null
    }
  ]
}
```

---

### `sweep_level`

Output-only: ramps amplitude linearly in dB from `start_dbfs` to `stop_dbfs`
over `duration` seconds at a fixed frequency. No capture.

**Request**
```json
{
  "cmd":        "sweep_level",
  "freq_hz":    <float>,
  "start_dbfs": <float>,
  "stop_dbfs":  <float>,
  "duration":   <float>   // seconds, default 1.0
}
```

**Reply**
```json
{ "ok": true, "out_port": "<resolved-jack-port>" }
```

On port error: `{ "ok": false, "error": "port error: ..." }`.

**DATA**
```json
// topic: done
{ "cmd": "sweep_level" }
```

---

### `sweep_frequency`

Output-only: logarithmic chirp from `start_hz` to `stop_hz` over `duration`
seconds at fixed level. No capture.

**Request**
```json
{
  "cmd":        "sweep_frequency",
  "start_hz":   <float>,
  "stop_hz":    <float>,
  "level_dbfs": <float>,
  "duration":   <float>   // seconds, default 1.0
}
```

**Reply**
```json
{ "ok": true, "out_port": "<resolved-jack-port>" }
```

**DATA**
```json
// topic: done
{ "cmd": "sweep_frequency" }
```

---

### `plot`

Blocking point-by-point frequency sweep: plays a tone at each frequency and
captures + analyses the loopback. Emits one `sweep_point` frame per frequency.

**Request**
```json
{
  "cmd":        "plot",
  "start_hz":   <float>,
  "stop_hz":    <float>,
  "level_dbfs": <float>,
  "ppd":        <int>,    // points per decade, default 10
  "duration":   <float>   // capture duration per point (seconds), default 1.0
}
```

**Reply**
```json
{ "ok": true, "out_port": "<port>", "in_port": "<port>" }
```

**DATA** — one per frequency:
```json
// topic: data  (sweep_point frame, see Shared types)
```

**DATA** — terminal:
```json
// topic: done
{ "cmd": "plot", "n_points": <int>, "xruns": <int> }
```

---

### `plot_level`

Blocking point-by-point level sweep at a fixed frequency. Plays and captures
at each level step. Emits one `sweep_point` frame per level.

**Request**
```json
{
  "cmd":        "plot_level",
  "freq_hz":    <float>,
  "start_dbfs": <float>,
  "stop_dbfs":  <float>,
  "steps":      <int>,    // default 26
  "duration":   <float>   // capture duration per point (seconds), default 1.0
}
```

**Reply**
```json
{ "ok": true, "out_port": "<port>", "in_port": "<port>" }
```

**DATA** — one per level step (sweep_point frame, `"cmd": "plot_level"`,
includes `"freq_hz"` and `"drive_db"` fields).

**DATA** — terminal:
```json
// topic: done
{ "cmd": "plot_level", "n_points": <int>, "xruns": <int> }
```

---

### `monitor_spectrum`

Continuous input-only spectrum monitor. Captures repeatedly at `interval`
seconds, auto-detects the dominant frequency, runs `analyze()`, and streams
spectrum frames until stopped.

**Request**
```json
{
  "cmd":        "monitor_spectrum",
  "freq_hz":    <float>,   // hint for initial fundamental; auto-detected thereafter
  "level_dbfs": <float>,   // unused by server (kept for client compat)
  "interval":   <float>    // capture + analysis interval (seconds), default 0.2
}
```

**Reply**
```json
{ "ok": true, "in_port": "<port>" }
```

**DATA** — repeated until stopped (spectrum frame, see Shared types).

**DATA** — terminal after `stop`:
```json
// topic: done
{ "cmd": "monitor_spectrum" }
```

---

### `generate`

Plays a continuous sine tone until stopped.

**Request**
```json
{
  "cmd":        "generate",
  "freq_hz":    <float>,
  "level_dbfs": <float>,
  "channels":   [<int>, ...]   // optional; defaults to configured output_channel
}
```

**Reply**
```json
{ "ok": true, "out_ports": ["<port>", ...] }
```

On port error: `{ "ok": false, "error": "port error: ..." }`.

**DATA** — terminal after `stop`:
```json
// topic: done
{ "cmd": "generate" }
```

---

### `generate_pink`

Plays continuous pink noise until stopped.

**Request**
```json
{
  "cmd":        "generate_pink",
  "level_dbfs": <float>,
  "channels":   [<int>, ...]   // optional
}
```

**Reply**
```json
{ "ok": true, "out_ports": ["<port>", ...] }
```

**DATA** — terminal after `stop`:
```json
// topic: done
{ "cmd": "generate_pink" }
```

---

### `calibrate`

Runs the interactive calibration procedure. Publishes `cal_prompt` frames
asking the client to enter DMM readings; client responds with `cal_reply`.

**Request**
```json
{
  "cmd":            "calibrate",
  "ref_dbfs":       <float>,   // optional, default -10.0
  "output_channel": <int>,     // optional, defaults to config
  "input_channel":  <int>      // optional, defaults to config
}
```

**Reply**
```json
{ "ok": true }
```

**DATA — `cal_prompt`** (step 1: output voltage):
```json
// topic: cal_prompt
{
  "step":     1,
  "text":     "<instructions for the user>",
  "dmm_vrms": <float> | null   // auto-read from DMM if configured
}
```

Client responds with `cal_reply` (see below).

**DATA — `cal_done`**:
```json
// topic: cal_done
{
  "key":               "out0_in0",
  "vrms_at_0dbfs_out": <float> | null,
  "vrms_at_0dbfs_in":  <float> | null,
  "error":             "<message>"   // only present on partial failure
}
```

---

### `cal_reply`

Sends the user's DMM reading back to a running `calibrate` worker.

**Request**
```json
{
  "cmd":  "cal_reply",
  "vrms": <float> | null   // null = skip / press Enter
}
```

**Reply**
```json
{ "ok": true }
```

---

### `dmm_read`

Takes 3 averaged AC Vrms readings from the configured Keysight 34461A DMM.

**Request**
```json
{ "cmd": "dmm_read" }
```

**Reply — success**
```json
{
  "ok":   true,
  "vrms": <float>,
  "idn":  "<IDN string>" | null
}
```

**Reply — no DMM configured**
```json
{ "ok": false, "error": "no DMM configured on server — run: ac setup dmm <host>" }
```

---

### `server_enable`

Rebinds both sockets to `tcp://*` (all interfaces) for remote access.
The reply is sent before the rebind happens.

**Request**
```json
{ "cmd": "server_enable" }
```

**Reply**
```json
{ "ok": true, "bind_addr": "*", "listen_mode": "public" }
```

---

### `server_disable`

Rebinds both sockets back to `tcp://127.0.0.1`.

**Request**
```json
{ "cmd": "server_disable" }
```

**Reply**
```json
{ "ok": true, "bind_addr": "127.0.0.1", "listen_mode": "local" }
```

---

### `server_connections`

Returns current listen mode and connected client endpoints.

**Request**
```json
{ "cmd": "server_connections" }
```

**Reply**
```json
{
  "ok":            true,
  "listen_mode":   "local" | "public",
  "ctrl_endpoint": "tcp://127.0.0.1:5556",
  "data_endpoint": "tcp://127.0.0.1:5557",
  "clients":       ["<endpoint>", ...],
  "workers":       ["<cmd-name>", ...]
}
```

---

## Busy guard

Audio commands are classified into three concurrency groups:

| Group | Commands |
|-------|---------|
| `OUTPUT` | `sweep_level`, `sweep_frequency`, `generate`, `generate_pink` |
| `INPUT`  | `monitor_spectrum` |
| `EXCLUSIVE` | `plot`, `plot_level`, `calibrate`, `transfer`, `probe`, `test_hardware`, `test_dut` |

Rules:
- Only one `OUTPUT` command at a time.
- Only one `INPUT` command at a time.
- An `EXCLUSIVE` command cannot start if **anything** is running.
- Nothing can start while an `EXCLUSIVE` command is running.

When the guard fires:
```json
{ "ok": false, "error": "busy: <running-cmd> running — send stop first" }
```

---

## Error handling

### Invalid JSON
```json
{ "ok": false, "error": "invalid JSON" }
```

### Unknown command
```json
{ "ok": false, "error": "unknown command: '<name>'" }
```

### Port out of range
```json
{ "ok": false, "error": "port error: Channel N out of range -- only M ports available: [...]" }
```

### Worker error (DATA frame)
```json
// topic: error
{ "cmd": "<name>", "message": "<exception string>" }
```

---

## Stale server detection

The Python server includes `"src_mtime"` in the `status` reply — the maximum
`mtime` of all `.py` files in `ac/server/`. The client uses this to detect if
the running server is older than the installed source and respawns it.

The Rust daemon should replace this with a build timestamp:

```json
{ "src_mtime": <unix-timestamp-float> }   // set to binary mtime at startup
```

The Python client compares the value but does not care about the source —
any float that changes when the server is rebuilt satisfies the contract.
