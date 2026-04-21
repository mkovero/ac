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

**PUB high-water mark.** The Rust daemon sets `ZMQ_SNDHWM = 50000` on the
DATA socket (default libzmq HWM is 1000). This is large enough to buffer a
full frequency sweep of `data` frames plus the terminal `done`/`error`/
`cal_done` frame even when the subscriber is lagging. If the internal
worker → main-loop channel ever accumulates more than 1000 pending frames
between main-loop drains the daemon logs `PUB backlog …` once so the
operator knows a subscriber is falling behind. Subscribers that exceed HWM
will still experience drops — clients must treat a missing terminal frame
as an error after the per-command deadline.

**DATA** frames are UTF-8 strings with a topic prefix:

```
<topic> <json-object>\n
```

e.g. `data {"type":"sweep_point", ...}` or `done {"cmd":"plot", ...}`.

Topics: `data`, `done`, `error`, `cal_prompt`, `cal_done`, `gpio`, `keepalive`.

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

## Tiered frame types (transition)

`ARCHITECTURE.md` introduces a two-tier model for the analysis stack:

- **Tier 1 — Reference measurement** (`measurement/…`): reproducible,
  archivable. Emitted by `plot` today; future `sweep`, `noise`, etc.
- **Tier 2 — Live analysis** (`visualize/…`): responsive,
  display-first. Emitted by `monitor_spectrum`.

During the transition window every live-analysis and per-point
measurement frame is published **twice**: once with the legacy `"type"`
value (e.g. `"sweep_point"`, `"spectrum"`), and once with the
tier-prefixed equivalent. The payloads are byte-identical apart from
the `"type"` field. Subscribers should prefer the tier-prefixed name
and treat the legacy copy as redundant.

| Tier  | Legacy `type`        | Tiered `type`                              |
|-------|----------------------|--------------------------------------------|
| 1     | `sweep_point`        | `measurement/frequency_response/point`     |
| 1     | — (new)              | `measurement/frequency_response/complete`  |
| 1     | — (new)              | `measurement/report`                       |
| 2     | `spectrum`           | `visualize/spectrum`                       |
| 2     | `cwt`                | `visualize/cwt`                            |
| 2     | `fractional_octave`  | `visualize/fractional_octave`              |

Legacy `type` names are scheduled for removal in `ac` v0.2.0.

### `measurement/report` frame

Emitted once at the end of a `plot` run. Carries the full archival
`MeasurementReport` JSON — the same shape written to
`cfg.report_dir/<ISO8601>-plot.json` when that directory is
configured. Schema is versioned (`schema_version: 1`); readers that
see an unknown version must refuse to decode. Example payload:

```json
{
  "type":   "measurement/report",
  "cmd":    "plot",
  "report": {
    "schema_version": 1,
    "ac_version":     "0.1.0",
    "timestamp_utc":  "2026-04-21T20:00:00Z",
    "method": {
      "kind":     "stepped_sine",
      "n_points": 3,
      "standard": { "standard": "IEC 60268-3:2018", "clause": "§14.12", "verified": false }
    },
    "stimulus":    { "sample_rate_hz": 48000, "f_start_hz": 100, "f_stop_hz": 10000, "level_dbfs": -20, "n_points": 3 },
    "integration": { "duration_s": 1.0, "window": "hann" },
    "data": {
      "kind":   "frequency_response",
      "points": [
        { "freq_hz": 100, "fundamental_dbfs": -20.1, "thd_pct": 0.005, "thdn_pct": 0.012, "noise_floor_dbfs": -120.0, "linear_rms": 0.0707, "clipping": false, "ac_coupled": false }
      ]
    }
  }
}
```

`data.kind` values currently defined:

| `kind`                 | Producer                                           | Payload shape                                                                     |
|------------------------|----------------------------------------------------|-----------------------------------------------------------------------------------|
| `frequency_response`   | `plot` (stepped-sine)                              | `{ "points": [FrequencyResponsePoint, ...] }`                                     |
| `spectrum_bands`       | IEC 61260-1 filterbank (`ac-core::measurement::filterbank`) | `{ "bpo": <int>, "class": "Class 1", "centres_hz": [...], "levels_dbfs": [...] }` |
| `impulse_response`     | Farina log-sweep (`ac-core::measurement::sweep`)   | `{ "sample_rate_hz": <int>, "f1_hz": <f>, "f2_hz": <f>, "duration_s": <f>, "linear_ir": [...], "harmonics": [{ "order": <int>, "samples": [...] }, ...] }` |

The `spectrum_bands` and `impulse_response` variants are serializable
today but not yet emitted from any CLI command — wiring is tracked in
issues #74 and #75.

### `measurement/frequency_response/complete` frame

Terminal summary for a `plot` run, emitted just before the
`measurement/report` frame. Carries only aggregate counters so a thin
subscriber can flag completion without parsing the full report:

```json
{ "type": "measurement/frequency_response/complete", "cmd": "plot", "n_points": <int>, "xruns": <int> }
```

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
  "channel":          <int>,          // input channel index this frame describes
  "n_channels":       <int>,          // total channels being monitored (frame count per cycle)
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

`channel` and `n_channels` were added alongside the optional `channels`
request parameter on `monitor_spectrum` (see below). Subscribers that only
track a single channel should filter by `channel == <their-channel>` — old
servers that do not emit either field should be treated as
`channel = 0, n_channels = 1`.

### `cwt` frame

Emitted continuously by `monitor_spectrum` when `analysis_mode` is `"cwt"`
(see `set_analysis_mode`). Replaces the `spectrum` frame one-for-one —
while CWT mode is active, no `spectrum` frames are published on the same
worker. Magnitudes are already in dBFS and frequencies are log-spaced, so
subscribers that expect a linear spectrum should convert / branch.

```json
{
  "type":        "cwt",
  "cmd":         "monitor_spectrum",
  "channel":     <int>,            // input channel index this column describes
  "n_channels":  <int>,            // total channels being monitored
  "sr":          <int>,            // sample rate (Hz)
  "magnitudes":  [<float>, ...],   // dBFS per scale, length = frequencies.len()
  "frequencies": [<float>, ...],   // Hz per scale, log-spaced
  "timestamp":   <int>,            // UNIX-epoch nanoseconds
  "xruns":       <int>
}
```

Default parameters (see `ac-core::cwt` constants): `σ = 12.0`,
`n_scales = 512`, frequency axis spans `20 Hz` to `0.9 · sr/2`.
Both `σ` and `n_scales` are tuneable at runtime via `set_analysis_mode`.

### `fractional_octave` frame

Emitted by `monitor_spectrum` **only when** `analysis_mode` is `"cwt"`
**and** `set_ioct_bpo` has been called with a non-zero bins-per-octave.
When enabled, one frame is published per channel per tick **in addition
to** the `cwt` frame: subscribers see two frames per channel back-to-back
(`cwt`, then `fractional_octave`). The aggregation reuses the same CWT
column the `cwt` frame carries — there is no second CWT cost.

```json
{
  "type":       "fractional_octave",
  "cmd":        "monitor_spectrum",
  "channel":    <int>,
  "n_channels": <int>,
  "sr":         <int>,
  "bpo":        <int>,            // bins per octave: 1, 3, 6, 12, or 24
  "freqs":      [<float>, ...],   // band centres (Hz), anchored at 1 kHz
  "spectrum":   [<float>, ...],   // dBFS per band
  "timestamp":  <int>,            // UNIX-epoch nanoseconds
  "xruns":      <int>
}
```

Band edges are half-band on each side of the centre
(`f_c · 2^(±1/(2·bpo))`). Aggregation sums per-band power across CWT
scales whose centre falls inside the band; bands that contain no scale
fall back to log-dB interpolation against the two nearest scales (mirrors
the FFT log-display fallback). **Not IEC 61260** — band shapes follow the
Morlet kernel, not standard third-octave filters; this is a visualization
feature, not a metrology filterbank. See `ac-core::fractional_octave` for
the algorithm and the documented kernel-overlap level drift on tones at
default CWT density.

### `keepalive` frame

Emitted once per second on the `keepalive` topic regardless of any running
worker. Clients can treat a stall in `seq` as a stalled daemon and a reset
to 1 as a daemon restart.

```json
{
  "type":      "keepalive",
  "seq":       <int>,                    // monotonic, resets to 1 on daemon start
  "timestamp": <int>,                    // UNIX-epoch nanoseconds
  "busy":      <bool>                    // any worker currently running?
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

### `set_analysis_mode`

Switches the spectrum analysis path used by `monitor_spectrum` between a
standard windowed FFT (default) and a Morlet continuous wavelet transform.
The mode is server-global; the next `monitor_spectrum` tick picks it up,
even if a `monitor_spectrum` worker is already running.

**Request**
```json
{ "cmd": "set_analysis_mode", "mode": "fft" }
{ "cmd": "set_analysis_mode", "mode": "cwt" }
{ "cmd": "set_analysis_mode", "mode": "cwt", "sigma": 12.0, "n_scales": 512 }
```

`sigma` (float, optional, clamped 5–24) and `n_scales` (int, optional,
clamped 64–2048) tune the Morlet wavelet shape and frequency-axis density.
Higher sigma = sharper frequency resolution, softer time resolution. More
scales = finer frequency grid. Both persist until changed or daemon restart.

**Reply**
```json
{ "ok": true, "mode": "fft" | "cwt", "sigma": <float>, "n_scales": <int> }
```

Unknown values for `mode` return `{ "ok": false, "error": "..." }` and
leave the current mode unchanged. Default at startup is `"fft"`.

---

### `get_analysis_mode`

Returns the current analysis mode.

**Request**
```json
{ "cmd": "get_analysis_mode" }
```

**Reply**
```json
{ "ok": true, "mode": "fft" | "cwt", "sigma": <float>, "n_scales": <int> }
```

---

### `set_ioct_bpo`

Toggles the per-tick `fractional_octave` frame published alongside the
`cwt` frame. Server-global; the next `monitor_spectrum` tick picks it up
even if a worker is already running. Has no effect in FFT mode (no `cwt`
column to aggregate). Persists across worker restart, resets to disabled
on daemon restart.

**Request**
```json
{ "cmd": "set_ioct_bpo", "bpo": 0 }    // disable (no extra frame)
{ "cmd": "set_ioct_bpo", "bpo": 3 }    // 1/3-octave
{ "cmd": "set_ioct_bpo", "bpo": 24 }   // 1/24-octave
```

`bpo` must be one of `0` (disable), `1`, `3`, `6`, `12`, `24`. Other
values reply `{ "ok": false, "error": "..." }` and leave the current
setting unchanged.

**Reply**
```json
{ "ok": true, "bpo": <int> }
```

---

### `stop`

Stops one or all running workers. **Synchronous**: the reply is only sent
after the targeted worker thread(s) have fully exited and been removed from
the busy map, so the very next command on the REP socket is guaranteed to see
a clean slate (e.g. issuing `transfer_stream` immediately after
`stop name=monitor_spectrum` will no longer be rejected as busy).

**Request**
```json
{ "cmd": "stop" }
{ "cmd": "stop", "name": "<worker-name>" }
```

`name` is optional. When omitted, all workers are stopped.

**Reply**
```json
{ "ok": true, "stopped": ["<worker-name>", ...] }
```

`stopped` lists the workers that were actually joined during this call —
empty if no matching worker was running.

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

Continuous input-only spectrum monitor. Auto-detects the dominant frequency,
runs `analyze()`, and streams spectrum frames until stopped. `interval`
controls the tick cadence (refresh rate); `fft_n` controls the FFT window
length (frequency resolution). The two are independent: for single-channel
monitoring the daemon maintains a sliding ring per channel, pulling only
`interval × sr` new samples per tick and analysing the trailing `fft_n`
window, so refresh can run faster than `fft_n / sr`. Multi-channel mode
captures a fresh `fft_n`-sample block per channel per tick (continuity
across `reconnect_input` can't be preserved).

**Request**
```json
{
  "cmd":        "monitor_spectrum",
  "freq_hz":    <float>,     // hint for initial fundamental; auto-detected thereafter
  "level_dbfs": <float>,     // unused by server (kept for client compat)
  "interval":   <float>,     // tick cadence (seconds), default 0.2
  "fft_n":      <int>,       // capture window = FFT N, power of 2 in [256, 131072]
                              // default: nearest pow2 of sr*interval (preserves legacy)
  "channels":   [<int>, ...] // optional; input channel indices to monitor
                              // defaults to [config.input_channel]
}
```

Both `interval` and `fft_n` are live-reconfigurable — see
`set_monitor_params` below.

When `channels` contains more than one index, the worker cycles through the
ports via `reconnect_input` (each channel gets `interval / N` seconds between
cycles; capture length per channel is still `fft_n` samples). Every
published `spectrum` frame carries distinct `channel` and `n_channels` fields
so subscribers can route frames independently. Backends whose
`reconnect_input` is a no-op (fake, CPAL) will emit N frames per cycle but
all drawn from the same live port.

**Reply**
```json
{
  "ok":       true,
  "in_port":  "<primary-port>",   // first channel — kept for backward compat
  "in_ports": ["<port>", ...],    // resolved port per entry in `channels`
  "channels": [<int>, ...]        // echoed channel indices (defaulted if absent)
}
```

**DATA** — repeated until stopped (spectrum frame, see Shared types).

**DATA** — terminal after `stop`:
```json
// topic: done
{ "cmd": "monitor_spectrum" }
```

---

### `set_monitor_params`

Live-tunes the running `monitor_spectrum` worker. Either or both of
`interval` and `fft_n` may be supplied; unspecified fields are left
unchanged. Takes effect on the next tick — no frame gap, no worker restart.

Request/reply is synchronous on the REP socket; no frames emitted.

**Request**
```json
{
  "cmd":      "set_monitor_params",
  "interval": <float>,   // optional; seconds, > 0
  "fft_n":    <int>      // optional; power of 2 in [256, 131072]
}
```

**Reply**
```json
{ "ok": true,  "interval": <float>, "fft_n": <int> }
{ "ok": false, "error": "no active monitor" }
{ "ok": false, "error": "fft_n must be power of 2 in [256, 131072]" }
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

### `transfer_stream`

Streaming H1 transfer function estimator. Captures the selected measurement
+ reference input channels and publishes a new `transfer_stream` frame each
iteration (every `ac_core::transfer::capture_duration(4, sr)` seconds,
≈ 2.5 s at 48 kHz) until stopped. Runs in the `TRANSFER` concurrency group:
only one `transfer_stream` at a time, but coexists with `monitor_spectrum`
(`INPUT`) and any `OUTPUT` worker (each owns its own JACK client).

By default the daemon is **passive** — it does not drive any output. The
caller is expected to feed stimulus (pink noise, music, sweep, speech, …) into
the DUT externally and the daemon just observes `ref` → `meas`. Pass
`drive=true` to restore the self-stimulating pink-noise loopback mode.

**Request**
```json
{
  "cmd":          "transfer_stream",
  "drive":        <bool>,    // optional, default false — if true, daemon plays pink noise on the output
  "level_dbfs":   <float>,   // only meaningful when drive=true, default -10

  // Either the multi-pair form …
  "pairs":        [[<meas0>, <ref0>], [<meas1>, <ref1>], ...],

  // … or the single-pair legacy form (still accepted):
  "meas_channel": <int>,     // capture port index for the measurement signal (DUT output)
  "ref_channel":  <int>      // capture port index for the reference signal (DUT input)
}
```

When `pairs` is present the daemon captures every unique channel referenced
in the list once per iteration and emits one `transfer_stream` DATA frame
per pair (each tagged with its own `meas_channel` / `ref_channel`). The
legacy single-pair form is equivalent to `pairs: [[meas_channel, ref_channel]]`.
`pairs` must be non-empty and every channel index must be within range.

**Reply**
```json
{
  "ok":           true,
  "out_port":     "<playback-port>",
  "pairs":        [[<meas0>, <ref0>], [<meas1>, <ref1>], ...],

  // For backward compatibility with single-pair callers the first pair is
  // also exposed at the top level:
  "meas_port":    "<capture-port>",
  "ref_port":     "<capture-port>",
  "meas_channel": <int>,
  "ref_channel":  <int>
}
```

**DATA** — one frame per pair per iteration, repeated until stopped:
```json
// topic: data
{
  "type":          "transfer_stream",
  "cmd":           "transfer_stream",
  "freqs":         [<float>, ...],     // up to 2000 points
  "magnitude_db":  [<float>, ...],
  "phase_deg":     [<float>, ...],
  "coherence":     [<float>, ...],
  "delay_samples": <int>,
  "delay_ms":      <float>,
  "meas_channel":  <int>,
  "ref_channel":   <int>,
  "sr":            <int>
}
```

**DATA** — terminal after `stop`:
```json
// topic: done
{ "cmd": "transfer_stream", "stopped": true }
```

---

## Busy guard

Audio commands are classified into four concurrency groups:

| Group | Commands |
|-------|---------|
| `OUTPUT`    | `sweep_level`, `sweep_frequency`, `generate`, `generate_pink` |
| `INPUT`     | `monitor_spectrum` |
| `TRANSFER`  | `transfer_stream` |
| `EXCLUSIVE` | `plot`, `plot_level`, `calibrate`, `probe`, `test_hardware`, `test_dut` |

Rules:
- Only one `OUTPUT` command at a time.
- Only one `INPUT` command at a time.
- Only one `TRANSFER` command at a time — but it coexists with `OUTPUT`
  and `INPUT` because each worker owns an independent JACK client.
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
