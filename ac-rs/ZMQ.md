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

e.g. `data {"type":"measurement/frequency_response/point", ...}` or `done {"cmd":"plot", ...}`.

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

## Tiered frame types

`ARCHITECTURE.md` defines a two-tier model for the analysis stack:

- **Tier 1 — Reference measurement** (`measurement/…`): reproducible,
  archivable. Emitted by `plot` today; future `sweep`, `noise`, etc.
- **Tier 2 — Live analysis** (`visualize/…`): responsive,
  display-first. Emitted by `monitor_spectrum`.

Every frame carries a tier-prefixed `"type"`:

| Tier  | `type`                                      |
|-------|---------------------------------------------|
| 1     | `measurement/frequency_response/point`      |
| 1     | `measurement/frequency_response/complete`   |
| 1     | `measurement/spectrum_bands`                |
| 1     | `measurement/impulse_response`              |
| 1     | `measurement/loudness`                      |
| 1     | `measurement/report`                        |
| 2     | `visualize/spectrum`                        |
| 2     | `visualize/cwt`                             |
| 2     | `visualize/cqt`                             |
| 2     | `visualize/reassigned`                      |
| 2     | `visualize/fractional_octave`               |
| 2     | `visualize/fractional_octave_leq`           |

**v0.2.0 (breaking)** — legacy unprefixed `type` names
(`sweep_point`, `spectrum`, `cwt`, `fractional_octave`) were removed.
External SUB subscribers must switch to the tier-prefixed names.

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
      "standard": { "standard": "IEC 60268-3:2018", "clause": "§15.12.3 Total harmonic distortion under standard measuring conditions", "verified": true }
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

`method.kind` values currently defined:

| `kind`          | Producer      | Extra fields                                                                                  |
|-----------------|---------------|-----------------------------------------------------------------------------------------------|
| `stepped_sine`  | `plot`        | `{ "n_points": <int>, "standard": <StandardsCitation?> }`                                     |
| `swept_sine`    | `plot_ir`     | `{ "f1_hz": <f>, "f2_hz": <f>, "duration_s": <f>, "standard": <StandardsCitation?> }`         |

`data.kind` values currently defined:

| `kind`                 | Producer                                           | Payload shape                                                                     |
|------------------------|----------------------------------------------------|-----------------------------------------------------------------------------------|
| `frequency_response`   | `plot` (stepped-sine)                              | `{ "points": [FrequencyResponsePoint, ...] }`                                     |
| `spectrum_bands`       | IEC 61260-1 filterbank (`ac-core::measurement::filterbank`) | `{ "bpo": <int>, "class": "Class 1", "centres_hz": [...], "levels_dbfs": [...] }` |
| `impulse_response`     | Farina log-sweep (`ac-core::measurement::sweep`)   | `{ "sample_rate_hz": <int>, "f1_hz": <f>, "f2_hz": <f>, "duration_s": <f>, "linear_ir": [...], "harmonics": [{ "order": <int>, "samples": [...] }, ...], "noise_tail_start_s": <f?> }` |
| `noise_result`         | AES17-2020 §6.4.2 idle-channel noise (`ac-core::measurement::noise`) | `{ "sample_rate_hz": <int>, "duration_s": <f>, "unweighted_dbfs": <f>, "a_weighted_dbfs": <f>, "ccir_weighted_dbfs": <f?> }` |
| `gated_frequency_response` | Time-gated FFT of the linear IR (`ac-core::measurement::sweep::gated_frequency_response`, #284) | `{ "points": [{ "freq_hz": <f>, "magnitude_db": <f>, "phase_deg": <f> }, ...] }` |

The `spectrum_bands`, `impulse_response`, and `noise_result` variants
are serializable today but not yet emitted from any CLI command —
wiring is tracked in issues #74, #75, and (for CCIR-468) #76.

### `measurement/frequency_response/complete` frame

Terminal summary for a `plot` run, emitted just before the
`measurement/report` frame. Carries only aggregate counters so a thin
subscriber can flag completion without parsing the full report:

```json
{ "type": "measurement/frequency_response/complete", "cmd": "plot", "n_points": <int>, "xruns": <int> }
```

### `measurement/spectrum_bands` frame

Emitted when a `plot` is run with `"bpo": <N>`. After the frequency-response
sweep finishes, the daemon feeds the concatenated capture through an
IEC 61260-1 Class 1 fractional-octave filterbank (`Filterbank::new(sr, bpo,
start_hz, stop_hz)`) and publishes the resulting per-band levels. A second
`measurement/report` follows with the full `SpectrumBands` payload.

```json
{
  "cmd":         "plot",
  "bpo":         <int>,           // bands per octave (1, 3, 6, 12, 24)
  "class":       "Class 1",
  "centres_hz":  [<float>, ...],  // band centre frequencies in Hz
  "levels_dbfs": [<float>, ...]   // per-band dBFS (AES17-2020 §3.12.1/§3.12.3 reference)
}
```

---

## Shared types

### `measurement/frequency_response/point` frame

Emitted by `plot` and `plot_level` for each measured frequency or level point.

```json
{
  "type":             "measurement/frequency_response/point",
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
  "vrms_at_0dbfs_in": <float> | null,
  // Processing-context envelope (#98): same shape Tier 2 monitor
  // frames carry. fundamental / harmonic / spectrum values reflect
  // the active mic-curve correction when `mic_correction == "on"`;
  // see #97 for the corrected fields.
  "mic_correction":   "on" | "off" | "none",
  "spl_offset_db":    <float> | null,
  "weighting":        "off" | "a" | "c" | "z",
  "time_integration": "off" | "fast" | "slow" | "leq",
  "smoothing_bpo":    <int> | null
}
```

### `spectrum` frame

Emitted continuously by `monitor_spectrum` when `analysis_mode == "fft"`
(default). The Tier-2-prefixed `type` is `visualize/spectrum`.

```json
{
  "type":             "visualize/spectrum",
  "cmd":              "monitor_spectrum",
  "channel":          <int>,          // input channel index this frame describes
  "n_channels":       <int>,          // total channels being monitored (frame count per cycle)
  "freq_hz":          <float>,        // auto-detected dominant frequency
  "sr":               <int>,          // sample rate (Hz)
  "freqs":            [<float>, ...], // downsampled, DC removed
  "spectrum":         [<float>, ...], // linear amplitude, one-sided, [0, 1] for bounded input — NOT dB
  "fundamental_dbfs": <float>,
  "thd_pct":          <float>,
  "thdn_pct":         <float>,
  "in_dbu":           <float> | null, // analog-domain level when voltage-cal'd
  "spl_offset_db":    <float> | null, // additive dBFS → dB SPL offset (calibration §)
  "mic_correction":   "on" | "off" | "none",   // mic frequency-response state
  "clipping":         <bool>,
  "xruns":            <int>
}
```

`channel` and `n_channels` were added alongside the optional `channels`
request parameter on `monitor_spectrum` (see below). Subscribers that only
track a single channel should filter by `channel == <their-channel>` — old
servers that do not emit either field should be treated as
`channel = 0, n_channels = 1`.

### `cwt` / `cqt` / `reassigned` frames

Emitted continuously by `monitor_spectrum` when `analysis_mode` is
`"cwt"` / `"cqt"` / `"reassigned"` (see `set_analysis_mode`). Each
mode replaces the `spectrum` frame one-for-one — while a Tier 2
spectral mode is active, no `visualize/spectrum` frames are published
on the same worker.

The three frames share the same payload shape (only `type` differs):
magnitudes are already in dBFS and frequencies are log-spaced, so
subscribers that expect a linear spectrum should convert / branch.

```json
{
  "type":           "visualize/cwt" | "visualize/cqt" | "visualize/reassigned",
  "cmd":            "monitor_spectrum",
  "channel":        <int>,            // input channel index this column describes
  "n_channels":     <int>,            // total channels being monitored
  "sr":             <int>,            // sample rate (Hz)
  "magnitudes":     [<float>, ...],   // dBFS per bin, length = frequencies.len()
  "frequencies":    [<float>, ...],   // Hz per bin, log-spaced
  "spl_offset_db":  <float> | null,   // additive dBFS → dB SPL offset (calibration §)
  "mic_correction": "on" | "off" | "none",   // mic-curve state for this channel
  "bpo":            <int>,            // CQT only: bins per octave used for this column
  "timestamp":      <int>,            // UNIX-epoch nanoseconds
  "xruns":          <int>
}
```

**Default parameters per mode**:

- **CWT** (`ac-core::visualize::cwt`): `σ = 12.0`, `n_scales = 512`,
  frequency axis spans `20 Hz` to `0.9 · sr/2`. Both `σ` and `n_scales`
  are runtime-tuneable via `set_analysis_mode`.
- **CQT** (`ac-core::visualize::cqt`): `bpo = 24` (24 bins per octave),
  `f_min = max(30 Hz, Q · sr / ring_len)`, `f_max = 0.9 · sr/2`. Daemon
  worker uses a 1.0 s ring (longer than CWT's 0.15 s) so the lowest
  bin's Q-invariant kernel fits.
- **Reassigned** (`ac-core::visualize::reassigned`): `n = 4096`,
  `n_out_bins = 1024`, log-spaced output grid from 20 Hz to
  `0.9 · sr/2`. Auger-Flandrin frequency reassignment with a 60 dB
  noise gate (bins below the column peak by that much keep their
  nominal frequency).

### `visualize/scope` frame

Emitted by `monitor_spectrum` once per channel per tick, **alongside**
the `visualize/{spectrum,cwt,cqt,reassigned}` frame for the same tick
(not instead of it). Carries raw f32 audio samples — no calibration,
no mic-curve, just the unmodified per-tick capture truncated to the
newest 2048 samples. Intended for a client-side goniometer / trajectory
view (`docs/superseded/unified.md` Phase 0b, resolves §9 OQ7); no current client
subscribes to it since the ac-ui detach.

```json
{
  "type":       "visualize/scope",
  "cmd":        "monitor_spectrum",
  "channel":    <int>,            // input channel index
  "n_channels": <int>,            // total channels being monitored
  "sr":         <int>,            // sample rate (Hz)
  "frame_idx":  <int>,            // monotonic per-tick counter (see below)
  "samples":    [<float>, ...],   // raw f32 in [-1, 1], length ≤ 2048
  "timestamp":  <int>,            // tick-wide UNIX-epoch nanoseconds
  "xruns":      <int>
}
```

**`frame_idx` synchronization.** The counter increments exactly once
per worker tick, so every channel's frame from the same capture tick
shares the same `frame_idx`. Subscribers that need a synchronized L/R
pair (Goniometer / PhaseScope3D) match frames by `frame_idx` rather
than relying on receive-order or `timestamp` (which is also tick-wide
but coarser).

**No calibration.** The trajectory consumers are dimensionless —
displaying a Lissajous figure of `(L, R)` doesn't need voltage or SPL
correction, and the mic-curve FIR adds compute / latency to a
already-large payload. Calibrated quantities live on the
`visualize/spectrum` (or `cwt` / etc.) frame for the same channel.

**Sample cap.** 2048 floats = 8 KB per frame per channel. At 192 kHz
× 200 ms tick the per-channel capture is ~38 k samples; we truncate
to the newest 2048 (~10 ms of audio at 192 kHz, which is plenty for a
60 fps render window). Visible aliasing on the Goniometer figure is
the failure mode that would prompt a v2 decimator.

**Bandwidth.** Worst case is 2 channels × 8 KB × 100 ticks/s
≈ 1.6 MB/s — comfortably within ZMQ inproc / localhost throughput
and below the existing `transfer_stream` payload size at long
sweep N.

### `fractional_octave` frame

Emitted by `monitor_spectrum` **only when** `analysis_mode` is `"cwt"`
**and** `set_ioct_bpo` has been called with a non-zero bins-per-octave.
When enabled, one frame is published per channel per tick **in addition
to** the `cwt` frame: subscribers see two frames per channel back-to-back
(`cwt`, then `fractional_octave`). The aggregation reuses the same CWT
column the `cwt` frame carries — there is no second CWT cost.

```json
{
  "type":           "visualize/fractional_octave",
  "cmd":            "monitor_spectrum",
  "channel":        <int>,
  "n_channels":     <int>,
  "sr":             <int>,
  "bpo":            <int>,            // bins per octave: 1, 3, 6, 12, or 24
  "weighting":      "off" | "a" | "c" | "z",
  "freqs":          [<float>, ...],   // band centres (Hz), anchored at 1 kHz
  "spectrum":       [<float>, ...],   // dBFS per band (post-weighting offset)
  "spl_offset_db":  <float> | null,   // additive dBFS → dB SPL (calibration §)
  "mic_correction": "on" | "off" | "none",
  "timestamp":      <int>,            // UNIX-epoch nanoseconds
  "xruns":          <int>
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

### `fractional_octave_leq` frame

Sidecar to the `fractional_octave` frame, emitted when the time-integration
mode is set to `fast` (τ = 125 ms), `slow` (τ = 1 s), or `leq` (unbounded
equivalent level). Subscribers see three frames per channel per tick:
`cwt`, then `fractional_octave`, then `fractional_octave_leq`. Toggled
via `set_time_integration`; Leq can be zeroed live via `reset_leq`.

```json
{
  "type":           "visualize/fractional_octave_leq",
  "cmd":            "monitor_spectrum",
  "channel":        <int>,
  "n_channels":     <int>,
  "sr":             <int>,
  "bpo":            <int>,
  "weighting":      "off" | "a" | "c" | "z",
  "mode":           "fast" | "slow" | "leq",
  "tau_s":          <float> | null,  // EMA time constant; null for leq
  "duration_s":     <float> | null,  // Leq-accumulator seconds; null for fast/slow
  "freqs":          [<float>, ...],  // band centres (Hz)
  "spectrum":       [<float>, ...],  // integrated dBFS per band
  "spl_offset_db":  <float> | null,
  "mic_correction": "on" | "off" | "none",
  "timestamp":      <int>,
  "xruns":          <int>
}
```

Same **not IEC 61672** caveat as the upstream `fractional_octave` frame:
the time constants and formulas match the standard but the band energies
come from a Morlet CWT aggregation, not an IEC 61260 filterbank. Display
use only.

### `measurement/loudness` frame

BS.1770-5 / EBU R128 loudness sidecar emitted by `monitor_spectrum`
once per channel per tick (alongside the spectrum-shaped frame above).
Tier 1 because the algorithms are standards-compliant; the values are
suitable for delivery-loudness checks (R128 ±0.5 LU integrated target).

```json
{
  "type":             "measurement/loudness",
  "cmd":              "monitor_spectrum",
  "channel":          <int>,
  "n_channels":       <int>,
  "sr":               <int>,
  "momentary_lkfs":   <float> | null,   // 400 ms window; null pre-gate
  "short_term_lkfs":  <float> | null,   // 3 s window
  "integrated_lkfs":  <float> | null,   // gated, since worker start (or last reset)
  "lra_lu":           <float>,          // EBU Tech 3342 loudness range
  "true_peak_dbtp":   <float> | null,   // 4× polyphase oversampled peak
  "gated_duration_s": <float>,          // wall-clock since gate first opened
  "spl_offset_db":    <float> | null,   // when set, UI renders LKFS as K-weighted dB SPL
  "timestamp":        <int>,
  "xruns":            <int>
}
```

Reset the integrated / LRA / true-peak accumulators live with the
`reset_loudness` command. The R128 PASS / WARN / FAIL anchor stays
on the raw `integrated_lkfs` regardless of `spl_offset_db` — the
−23 LKFS target is independent of the absolute reference.

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
  "server_enabled": <bool>,
  "home":           "<path>",         // this process's $HOME ("." if unset) — #385
  "config_path":    "<path>",         // config.json path in use
  "pid":            <int>,
  "started_at":     "<RFC3339>",      // UTC, second precision — process start
  "spawn_mode":     "auto" | "manual" // "auto" = started via ac-cli's spawn_daemon()
}
```

`home`/`config_path`/`pid`/`started_at`/`spawn_mode` (#385) let a client tell
this daemon apart from another one squatting the same hardcoded 5556/5557
ports under a different `HOME` — e.g. a leftover auto-spawn from an isolated
test/rig run. Additive fields; a client that doesn't read them is unaffected.

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

Switches the spectrum analysis path used by `monitor_spectrum` between
windowed FFT (default), Morlet CWT, constant-Q transform, or
Auger-Flandrin reassigned STFT. The mode is server-global; the next
`monitor_spectrum` tick picks it up, even if a `monitor_spectrum`
worker is already running.

**Request**
```json
{ "cmd": "set_analysis_mode", "mode": "fft" }
{ "cmd": "set_analysis_mode", "mode": "cwt" }
{ "cmd": "set_analysis_mode", "mode": "cqt" }
{ "cmd": "set_analysis_mode", "mode": "reassigned" }
{ "cmd": "set_analysis_mode", "mode": "cwt", "sigma": 12.0, "n_scales": 512 }
```

`sigma` (float, optional, clamped 5–24) and `n_scales` (int, optional,
clamped 64–8192) tune the Morlet wavelet shape and frequency-axis density.
Higher sigma = sharper frequency resolution, softer time resolution. More
scales = finer frequency grid. Both persist until changed or daemon restart.
The `cqt` and `reassigned` modes use fixed defaults (see the frame
section above); runtime tunables for those are a follow-up.

**Reply**
```json
{ "ok": true, "mode": "fft" | "cwt" | "cqt" | "reassigned",
  "sigma": <float>, "n_scales": <int> }
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
{ "ok": true, "mode": "fft" | "cwt" | "cqt" | "reassigned",
  "sigma": <float>, "n_scales": <int> }
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

### `set_band_weighting`

Sets the frequency-weighting curve applied to every band level before
the daemon emits the `fractional_octave` / `fractional_octave_leq`
frames. Adds the IEC 61672-1 Annex E dB offset at each band centre.

**Request**
```json
{ "cmd": "set_band_weighting", "mode": "off" | "a" | "c" | "z" }
```

- `off` — identity (no offset applied; default at startup).
- `a` — A-weighting per IEC 61672-1 Annex E.
- `c` — C-weighting per IEC 61672-1 Annex E.
- `z` — explicitly flat (identity); distinct from `off` only so UI
  affordances can distinguish "user hasn't picked" from "user picked Z".

Case-insensitive. Unknown values reply `{ "ok": false, ... }` and leave
the current setting unchanged.

**Reply**
```json
{ "ok": true, "mode": "a" }
```

Note: same caveat as the upstream `fractional_octave` frame — Morlet
CWT aggregation is not an IEC 61260 filterbank, so the weighted output
is display-only and must not be quoted as an IEC 61672 SPL reading.

---

### `get_band_weighting`

Returns the current weighting curve.

**Request**
```json
{ "cmd": "get_band_weighting" }
```

**Reply**
```json
{ "ok": true, "mode": "off" | "a" | "c" | "z" }
```

---

### `set_time_integration`

Sets the per-band time-integration mode applied to the live
`fractional_octave` frame. When non-`off`, the monitor worker publishes
an additional `fractional_octave_leq` frame after each `fractional_octave`
frame carrying the integrated per-band dBFS values. Server-global;
picked up by the next monitor tick without a worker restart. Setting
`leq` clears any existing Leq accumulator on the next tick.

**Request**
```json
{ "cmd": "set_time_integration", "mode": "off" | "fast" | "slow" | "leq" }
```

- `off` — no sidecar frame (default at startup).
- `fast` — exponentially-weighted average, τ = 125 ms.
- `slow` — exponentially-weighted average, τ = 1 s.
- `leq` — unbounded cumulative equivalent level; reset explicitly via `reset_leq`.

Mode is case-insensitive. Unknown values reply `{ "ok": false, ... }` and
leave the current setting unchanged.

**Reply**
```json
{ "ok": true, "mode": "fast" }
```

Note: the Morlet CWT aggregation upstream of the integrator is not an
IEC 61260 filterbank — the mode names mirror IEC 61672 time constants but
the output is display-only and must not be quoted as SPL.

---

### `get_time_integration`

Returns the current time-integration mode.

**Request**
```json
{ "cmd": "get_time_integration" }
```

**Reply**
```json
{ "ok": true, "mode": "off" | "fast" | "slow" | "leq" }
```

---

### `reset_leq`

Zeros the Leq accumulators on the next monitor tick. Safe to call when
no monitor is active — the flag is latched until a worker consumes it.
Fast/slow modes ignore the flag (they re-prime from the next input on
their own).

**Request**
```json
{ "cmd": "reset_leq" }
```

**Reply**
```json
{ "ok": true }
```

---

### `reset_loudness`

Zeros the per-channel BS.1770-5 loudness state (integrated LKFS, LRA,
dBTP) on the next monitor tick. Momentary and short-term windows are not
cleared by this — they re-prime from their next input on their own. Safe
to call when no monitor is active: the flag is one-shot and held until a
worker consumes it.

**Request**
```json
{ "cmd": "reset_loudness" }
```

**Reply**
```json
{ "ok": true }
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
  "reference_port":    "<sticky-name>" | null,
  "reference_output_channel": <int> | null,
  "reference_output_port":    "<sticky-name>" | null
}
```

`reference_channel` / `reference_port` index the **capture** list;
`reference_output_channel` / `reference_output_port` index the **playback**
list. They are independent settings — neither is derived from the other.

### `warnings` (optional, any reply)

A successful reply may carry `"warnings": ["<text>", ...]` — advisories that do
not make the command fail. `transfer_stream`, `test_hardware` and `test_dut`
emit one when `reference_channel` is set and `reference_output_channel` is not:
before #225 that config drove the reference stimulus out
`playback[reference_channel]`, and it now leaves on the main output, so a rig
whose loopback sat at that index worked before and does not now.

The field is absent, never an empty array, when there is nothing to say. It is
repeated on every reply rather than sent once, so a client connecting to an
already-running daemon still receives it.

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
    "reference_channel": <int>,     // optional — capture index
    "reference_output_channel": <int> | null,  // optional — playback index;
                                    //   null = reference leaves on the main output
    "dbu_ref_vrms":      <float>,   // optional
    "dmm_host":          "<host>" | null,  // optional
    "server_enabled":    <bool>,    // optional
    "backend":           "jack" | "sounddevice" | null,  // optional
    "snapshot_ring_s":   <float>,   // optional, > 0 — see `snapshot`
    "snapshot_spool_dir":"<path>" | null  // optional — see `snapshot`
  }
}
```

When `output_channel`, `input_channel`, `reference_channel`, or
`reference_output_channel` is updated, the server clears that leg's sticky
port name so the new channel takes effect; the port is re-resolved on next
use. Updating `reference_channel` never moves the reference *output* leg, and
vice versa.

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
  "ok":                                true,
  "found":                             true,
  "key":                               "out0_in0",
  "vrms_at_0dbfs_out":                 <float> | null,
  "vrms_at_0dbfs_in":                  <float> | null,
  "ref_dbfs":                          <float>,
  "mic_sensitivity_dbfs_at_94db_spl":  <float> | null,    // SPL cal layer
  "mic_response": {                                        // mic-curve layer
    "freqs_hz":    [<float>, ...],
    "gain_db":     [<float>, ...],
    "source_path": "<string>" | null,
    "imported_at": "<RFC3339>"
  } | null,
  "tau_history": [                                          // interface-latency (τ) layer, #281/#297
    {
      "conditions": {
        "device":      <int>,
        "backend":     "<string>",
        "sample_rate": <int>,
        "period_size": <int> | null,   // null = backend cannot report one
        "output_port": "<string>",
        "input_port":  "<string>"
      },
      "tau_s":       <float>,
      "measured_at": "<RFC3339>",
      "method":      "<string>",
      "agreement_count": <int>   // #347: readings that corroborated this entry, always >= 2; 0 on any entry written before #347 (`#[serde(default)]`) — never indistinguishable from a corroborated one
    }
  ]   // always present, [] when no τ has ever been measured for this key
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
      "key":                               "out0_in0",
      "vrms_at_0dbfs_out":                 <float> | null,
      "vrms_at_0dbfs_in":                  <float> | null,
      "mic_sensitivity_dbfs_at_94db_spl":  <float> | null,
      "mic_response":                      { ... } | null,
      "tau_history":                       [ { ... } ]   // same shape as get_calibration, always present, [] when unmeasured
    }
  ]
}
```

---

### `sweep_level`

Output-only: ramps amplitude linearly in dB from `start_dbfs` to `stop_dbfs`
over `duration` seconds at a fixed frequency. No capture.

Reached from the CLI via `ac generate level` (#282; `ac sweep level` is a
deprecated alias). The wire `cmd` is unchanged — `sweep_level` was always an
accurate name for what this command does (ramp output, no capture); only
its CLI parent noun moved.

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

`start_dbfs`/`stop_dbfs` are clamped to the config's `drive_max_dbfs`
ceiling (#360) — each point on the ramp individually, not just the
endpoints, so a range whose top end exceeds the ceiling flattens there
rather than running unclamped.

**Reply**
```json
{ "ok": true, "out_port": "<resolved-jack-port>", "start_dbfs": <float>, "stop_dbfs": <float> }
```

`start_dbfs`/`stop_dbfs` in the reply are the *applied* endpoints — what
was requested clamped to the ceiling — same convention as `set_drive`'s
`level_dbfs` echo.

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

Reached from the CLI via `ac generate frequency` (#282; `ac sweep frequency`
is a deprecated alias). Wire `cmd` unchanged — same reasoning as
`sweep_level` above.

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

`level_dbfs` is clamped to the config's `drive_max_dbfs` ceiling (#360).

**Reply**
```json
{ "ok": true, "out_port": "<resolved-jack-port>", "level_dbfs": <float> }
```

`level_dbfs` in the reply is the applied value after the clamp — same
convention as `set_drive`.

**DATA**
```json
// topic: done
{ "cmd": "sweep_frequency" }
```

---

### `plot_ir`

Farina exponential log-sweep impulse-response measurement. Generates an
ESS, plays it out the configured output, synchronously captures
`duration + tail_s` of the measurement input, and deconvolves via the
normalized inverse filter (`ac-core::measurement::sweep`). Emits a
`measurement/impulse_response` frame and a full `measurement/report`.

Renamed from `sweep_ir` by #282: reached from the CLI via `ac plot ir`
(`ac sweep ir` is a deprecated alias) — it captures and analyses, so it
now belongs with its `plot`/`plot_level` siblings, both in the CLI and on
the wire, rather than parked under a generator name. All three DATA
topics' `"cmd"` field and the busy-guard group key moved with it (see the
`EXCLUSIVE` group table below).

`report.notes` carries the ISO 18233 §6.3.2 post-hoc tail-decay verdict:
whether the captured tail decayed at least 30 dB in every 1/3-octave band
between the linear-IR peak and the end of the capture — a room's
reverberation isn't known ahead of a real capture, so `tail_s`'s adequacy
is checked after the fact against the room actually measured, not
guessed beforehand (`ac_core::measurement::sweep::check_tail_decay`).

Fake, JACK, and CPAL backends all implement the required
`play_and_capture` engine path (`jack_backend.rs`, `cpal_backend.rs`) —
only the default trait impl bails.

**Request**
```json
{
  "cmd":          "plot_ir",
  "f1_hz":        <float>,   // default 20
  "f2_hz":        <float>,   // default 20000 (must be < sr/2)
  "duration":     <float>,   // seconds, default 1.0
  "level_dbfs":   <float>,   // default -6
  "tail_s":       <float>,   // extra capture beyond sweep end, default 0.5
  "n_harmonics":  <int>,     // default 5
  "window_len":   <int>      // requested IR gate length in samples, default 4096
}
```

`level_dbfs` is clamped to the config's `drive_max_dbfs` ceiling (#360) —
before #360 this was the one command besides `calibrate` that emitted
whatever was asked for with nothing bounding it.

`window_len` is a request, not a guarantee. Gates for adjacent harmonic
orders must not overlap, so each order's gate is clamped down to the
sample distance to its nearest neighbouring order; the linear IR, whose
only neighbour is order 2, is usually left at the requested length while
the tighter high orders are shortened. The lengths actually used come
back on the `measurement/impulse_response` frame, and any shortening is
also stated in the report `notes`.

**Reply**
```json
{ "ok": true, "out_port": "<resolved-output-port>", "level_dbfs": <float> }
```

`level_dbfs` in the reply is the applied (clamped) level, and the report's
`stimulus.level_dbfs` field (below) is the same applied value.

**DATA**
```json
// topic: measurement/impulse_response
// window_len_used is indexed by order-1: [0] is the linear IR (order 1),
// [i] is harmonic order i+1. Each entry is the gate length of the
// corresponding IR in `data`, which may be below window_len_requested.
{ "cmd": "plot_ir", "data": { "kind": "impulse_response", ... },
  "window_len_requested": 4096, "window_len_used": [4096, 2818, 1999, 1551, 1551] }

// topic: measurement/report
{ "cmd": "plot_ir", "report": { "schema_version": 5, "notes": "ISO 18233 §6.3.2 ...\nThe decaying tail ... §B.5.", "interface_latency": { ... }, ... } }

// topic: done
{ "cmd": "plot_ir" }
```

`notes` carries independent statements, **one per line**: the ISO 18233
§6.3.2 tail-decay verdict *measured* from this capture; the #278 gate
clamp note when any order was shortened; and the §B.5 statement that the
tail past the peak is a linear-deconvolution artefact — a structural
consequence of `deconvolve_full`, true regardless of what the other two
came back as. None implies the others; a reader must not collapse them,
and must split on `\n` rather than assuming a single sentence.

The IR payload carries a `gate` block (`gate_start_s`, `gate_length_s`,
`window_kind: "rectangular"`, `f_low_hz`) describing the window
`extract_irs` applied to the *linear* IR, centred on the sweep endpoint.
Its length is `window_len_used[0]` — the length order 1 actually got
after #278's per-order clamp — not the requested `window_len`, which the
clamp may have shortened. `f_low_hz` follows from that same length.
The `ImpulseResponse` payload also carries `noise_tail_start_s`: seconds
after the linear-IR peak past which the capture can only hold
linear-deconvolution noise, never real system response (derived from the
sweep duration, not estimated — see `ac-core::measurement::sweep::noise_tail_start_s`).

`report.data` (#284) carries a **second** payload alongside the impulse
response: a `gated_frequency_response` derived by Tukey-tapering
(`window_kind: "tukey0.25"`) and FFTing the linear IR, gated to start at
the IR's peak-reference sample (`gate_start_s: 0.0`, measured from the
peak — not from the buffer origin, unlike the `ImpulseResponse` payload's
own `gate` block) and run for half the linear gate's length. Its `gate`
block's `f_low_hz` is `1 / gate_length_s` for *that* gate, independent of
the `ImpulseResponse` payload's own `f_low_hz`. Cites both
`ac-core::measurement::sweep::farina_citation()` (Farina basis only — no
ISO 18233 half, since a quasi-anechoic capture has no classical-standard
counterpart to pair it with) and
`ac-core::measurement::sweep::gated_response_citation()` (AES17-2020
Annex A.4.5, the gating method itself).

`interface_latency` is the τ (interface round-trip latency) resolved for
this capture — the field that lets an arrival be converted to a
τ-corrected flight time (milliseconds; #391 removed the further ms → m
conversion this used to also unlock). It is a tagged union on `state`:

```json
// τ measured under exactly these conditions (device, backend, sample
// rate, period size, port pair) by a prior `calibrate` run
{ "state": "measured", "tau_s": 0.0011931, "measured_at": "<RFC3339>",
  "method": "farina_short_ess", "backend": "jack", "sample_rate_hz": 48000,
  "period_size": 1024, "output_port": "...", "input_port": "..." }

// no exact match — never a nearest-neighbour or interpolated value
{ "state": "unavailable", "reason": "no τ entry for these exact conditions; nearest stored entry (measured ...) differs in period_size (requested 512, stored 1024)" }
```

A reader must not subtract τ from the arrival when `state` is
`unavailable`: the arrival still contains the uncorrected interface
latency, which at 48 kHz is routinely tens of samples of phantom path.

When `cfg.report_dir` is configured the daemon also writes the pair
`<ISO8601>-plot_ir.json` and `<ISO8601>-plot_ir.csv` there (colons
replaced with `-`), so the result survives the run without the client
having to save anything.

---

### `plot`

Blocking point-by-point frequency sweep: plays a tone at each frequency and
captures + analyses the loopback. Emits one `measurement/frequency_response/point` frame per frequency.

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

`level_dbfs` is clamped to the config's `drive_max_dbfs` ceiling (#360).

**Reply**
```json
{ "ok": true, "out_port": "<port>", "in_port": "<port>", "level_dbfs": <float> }
```

`level_dbfs` in the reply is the applied value; each
`measurement/frequency_response/point` frame's `drive_db` (below) is the
same applied value.

**DATA** — one per frequency:
```json
// topic: data  (measurement/frequency_response/point frame, see Shared types)
```

**DATA** — terminal:
```json
// topic: done
{ "cmd": "plot", "n_points": <int>, "xruns": <int> }
```

---

### `plot_level`

Blocking point-by-point level sweep at a fixed frequency. Plays and captures
at each level step. Emits one `measurement/frequency_response/point` frame per level.

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

`start_dbfs`/`stop_dbfs` are clamped to the config's `drive_max_dbfs`
ceiling (#360), each computed step individually — a range whose top end
exceeds the ceiling flattens there rather than running unclamped.

**Reply**
```json
{ "ok": true, "out_port": "<port>", "in_port": "<port>", "start_dbfs": <float>, "stop_dbfs": <float> }
```

`start_dbfs`/`stop_dbfs` in the reply are the applied endpoints.

**DATA** — one per level step (measurement/frequency_response/point frame, `"cmd": "plot_level"`,
includes `"freq_hz"` and `"drive_db"` fields — `drive_db` is the applied,
post-clamp level for that step).

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
  "ok":           true,
  "in_port":      "<primary-port>", // first channel — kept for backward compat
  "in_ports":     ["<port>", ...],  // resolved port per entry in `channels`
  "channels":     [<int>, ...],     // echoed channel indices (defaulted if absent)
  "lf_fft_n":     <int>,            // dual-resolution low-band FFT N (see below)
  "crossover_hz": <float>           // LF/HF split frequency (daemon-owned constant)
}
```

`lf_fft_n` and `crossover_hz` are daemon-owned constants for the
dual-resolution low-frequency path (see below). They are read-only and
echoed so the UI can label the LF band without hardcoding daemon values.

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
{ "ok": true,  "interval": <float>, "fft_n": <int>, "lf_fft_n": <int>, "crossover_hz": <float> }
{ "ok": false, "error": "no active monitor" }
{ "ok": false, "error": "fft_n must be power of 2 in [256, 131072]" }
```

`lf_fft_n` / `crossover_hz` are echoed read-only (the UI cannot set them);
only `interval` and `fft_n` are user-tunable.

---

### Dual-resolution low-frequency path (#142)

Linear FFT bins are equally spaced, so the bottom octaves get few bins and
closely-spaced low tones smear together. To split tones ~5 Hz apart below
100 Hz you need `Δf ≲ 2.5 Hz`, i.e. `N ≥ 32768` at 48 kHz — but applying that
window to the whole band would add ~1.4 s of latency everywhere and smear
mid/high transients.

Instead the FFT monitor runs a **second, longer** FFT (`lf_fft_n`, default
65536 ≈ 0.73 Hz Δf at 48 kHz) over the same capture ring and uses it **only
below `crossover_hz`** (default 750 Hz). The live `fft_n` keeps driving
everything above the crossover at the normal refresh rate. The two
linear-amplitude half-spectra are merged into the single log-column
`spectrum` wire frame (`spectrum_to_columns_multiband`), cross-faded linearly
in linear amplitude across a ±1/6-octave band at the crossover so the splice
is seamless. **The wire frame shape is unchanged** — subscribers need no
changes.

Resolution / trade-off the user gets:

- **Below `crossover_hz`:** `Δf = sr / lf_fft_n` (≈ 0.73 Hz at 48 kHz),
  enough to separate 5 Hz-spaced tones under 100 Hz. The LF band inherently
  refreshes at the long-block rate (`lf_fft_n / sr` ≈ 1.4 s) — acceptable
  because LF content is slow-moving. The long FFT is recomputed at most once
  per block duration to bound CPU.
- **Above `crossover_hz`:** unchanged — `Δf = sr / fft_n` at the live refresh
  rate, so mid/high responsiveness is not degraded.

The LF path is **inactive** whenever `fft_n >= lf_fft_n` (the live spectrum is
already at least as fine), in which case the monitor behaves exactly as
before. Peak detection (`peaks` field) uses the LF spectrum below the
crossover and the live spectrum above it. The UI's top-right readout shows
both resolutions on two lines, each scoped by the crossover, when the LF path
is active.

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

`level_dbfs` is clamped to the config's `drive_max_dbfs` ceiling (#360).

**Reply**
```json
{ "ok": true, "out_ports": ["<port>", ...], "level_dbfs": <float> }
```

`level_dbfs` in the reply is the applied value.

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

`level_dbfs` is clamped to the config's `drive_max_dbfs` ceiling (#360).

**Reply**
```json
{ "ok": true, "out_ports": ["<port>", ...], "level_dbfs": <float> }
```

`level_dbfs` in the reply is the applied value.

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
  "ref_dbfs":       <float>,   // optional, default: the session's drive_max_dbfs
  "output_channel": <int>,     // optional, defaults to config
  "input_channel":  <int>      // optional, defaults to config
}
```

`ref_dbfs` is clamped to the config's `drive_max_dbfs` ceiling (#360) —
an omitted value now defaults to that same ceiling rather than a
hardcoded -10.0, so it can never itself sit above the config it is
supposed to respect.

**Reply**
```json
{ "ok": true, "ref_dbfs": <float> }
```

`ref_dbfs` in the reply is the applied (clamped, or defaulted) value.

**DATA — `cal_prompt`** (step 1: output voltage at the DAC, while a
1 kHz tone is playing at `ref_dbfs`):
```json
// topic: cal_prompt
{
  "step":     1,
  "text":     "<instructions for the user>",
  "dmm_vrms": <float> | null,  // auto-read from DMM if configured
  "ref_dbfs": <float>          // peak-referenced dBFS of the played tone
}
```

**DATA — `cal_prompt`** (step 2: input voltage at the ADC; the tone
keeps playing so the daemon can also read the captured input dBFS):
```json
// topic: cal_prompt
{
  "step":          2,
  "text":          "<instructions for the user>",
  "dmm_vrms":      <float> | null, // DMM read; or step 1's reading when loopback
  "captured_dbfs": <float>,        // RMS-referenced dBFS captured at the ADC
  "loopback":      <bool>          // true → captured ≈ ref_dbfs - 3.01 dB,
                                   //         step 1's reading is pre-filled
}
```

Both prompts: client responds with `cal_reply` (see below). The user
enters the analog Vrms read from a DMM at the relevant point; the
daemon converts to "Vrms at 0 dBFS peak" before saving:
- `vrms_at_0dbfs_out = reading / dbfs_to_amplitude(ref_dbfs)`
- `vrms_at_0dbfs_in  = reading / dbfs_to_amplitude(captured_dbfs)`

A prompt answered with a skip (`vrms: null`, no `clear`) leaves the
stored value for that leg alone — re-checking one leg does not cost the
other. Only an explicit `clear: true` erases. `ref_dbfs` on the stored
entry is rewritten only when at least one leg was actually measured, so
it keeps describing the level the stored readings were taken at.

A timed-out prompt (120 s, no reply) resolves the same as a skip and the
run continues — the other leg can still be answered and saved normally.
A **cancelled** prompt (`{"cmd": "stop"}`, sent by the CLI's `q`) is not
a skip: it aborts the whole run before anything is saved, at either
step, so a cancel after step 1 was answered does not commit that
reading either.

**DATA — `cal_done`**:
```json
// topic: cal_done
{
  "key":                  "out0_in0",
  "vrms_at_0dbfs_out":    <float> | null,  // post-scale, projected to 0 dBFS
  "vrms_at_0dbfs_in":     <float> | null,  // post-scale, projected to 0 dBFS
  "out_state":            "measured" | "unchanged" | "absent",
  "in_state":             "measured" | "unchanged" | "absent",
  "tau_state":            "measured" | "not_measured_no_loopback" | "error"
                           | "disagree_period_shift" | "disagree_other",
  "tau_s":                <float> | null,  // interface round-trip delay, seconds; only non-null when tau_state == "measured"
  "tau_sample_rate":      <int>,           // condition τ was measured/attempted under
  "tau_period_size":      <int> | null,    // ditto; null on backends that can't report one (not "unknown")
  "tau_agreement_count":  <int>,           // #347: readings that agreed; 0 unless tau_state == "measured", where it is always 2
  "tau_reading1_s":       <float>,         // #347: first lifecycle's raw reading — present whenever both lifecycles ran (measured / disagree_*)
  "tau_reading2_s":       <float>,         // #347: second lifecycle's raw reading — ditto
  "tau_delta_samples":    <int>,           // #347: round((reading2 - reading1) * sample_rate) — present only on disagree_*
  "tau_periods":          <int>,           // #347: signed period count — present only on tau_state == "disagree_period_shift"
  "tau_error":            "<message>",     // present when tau_state is "error", "disagree_period_shift", or "disagree_other"
  "error":                "<message>",     // only present on partial failure (voltage-cal save)
  "input_port":           "<port>",        // #370: resolved server-side, e.g. "system:capture_2" — not the client's copy of the request
  "output_port":          "<port>"         // ditto, e.g. "system:playback_5"
}
```

`input_port` / `output_port` are what `resolve_input`/`resolve_output` actually
resolved this run — the same values the tone was played on and the capture
was taken from. A config edit between two `calibrate` runs against one
long-lived (auto-spawned) daemon changes these on the very next run (#370);
before this field existed the two runs were indistinguishable on the wire,
which is how a channel scan against one daemon read ten identical results
as the physical answer instead of the daemon never having reloaded.

The `vrms_at_0dbfs_*` fields report what is **stored after this run**,
not only what this run measured, and the `*_state` word says which:

| state | meaning |
|-------|---------|
| `measured` | this run supplied a reading and overwrote the field |
| `unchanged` | the prompt was skipped; the previously stored value stands |
| `absent` | the field holds no value — never set, or just cleared |

**τ (interface latency, #281/#347)** is not prompt-driven — it piggybacks
on the loopback state `cal_prompt` step 2 already established, so there is
no third interactive step and no `unchanged` state (skipping a voltage
prompt does not affect it). #347: a single reading is not a measurement of
τ on this stack — round-trip latency for a fixed path can vary by exactly
one period between client lifetimes, invisible within any one lifetime
(stable to 0.001 frames). `calibrate` therefore always runs τ as **two**
independent client lifecycles (fresh `start`/`stop` each) and compares
them before storing anything:

| `tau_state` | meaning |
|-------------|---------|
| `measured` | loopback detected this run; two independent readings agreed to the whole sample and their average was appended to `tau_history` |
| `not_measured_no_loopback` | loopback not detected this run — nothing to measure τ against |
| `error` | loopback was detected but a lifecycle's own measurement failed (`tau_error` names why, including which reading); the voltage-cal legs above are unaffected |
| `disagree_period_shift` | the two readings disagreed by an exact multiple of `tau_period_size` samples — a graph-buffering shift (software), not hardware drift. Nothing is stored. |
| `disagree_other` | the two readings disagreed, but not by a period multiple — a different fault class. Nothing is stored. |

`tau_sample_rate` / `tau_period_size` are the conditions the attempt ran
under (present regardless of `tau_state`, including `error`), so a
`not_measured_no_loopback` or `error` result is still legible against
`cal.json` history without a second round trip. `tau_period_size: null`
does not mean unknown — see `AudioEngine::period_size` in
`ac-daemon/src/audio/mod.rs`: some backends cannot report a period size
at all, which is a documented backend limitation, distinct from a period
size that simply wasn't queried (and means `disagree_period_shift` can
never fire on that backend — any disagreement there is `disagree_other`).

On either disagreement state, `tau_reading1_s` / `tau_reading2_s` are the
raw seconds values from the two lifecycles, shown verbatim rather than
compressed to a delta — the fractional part staying identical across a
period-shift jump is the diagnostic clue #347 itself was found from.
`tau_delta_samples` and (on `disagree_period_shift`) `tau_periods` are
the already-classified delta, so a client doesn't have to re-derive the
rounding/period-multiple logic itself.

---

### `cal_reply`

Sends the user's DMM reading back to a running `calibrate` worker.
Also used as a sync point by `calibrate_spl` (the `vrms` field is
ignored there — only the act of replying releases the worker).

Three distinct intents, not two — skipping a leg and erasing it are
different requests and must not share an encoding:

| request | intent |
|---------|--------|
| `{"vrms": <float>}` | measured reading; overwrites the stored value |
| `{"vrms": null}` (or `vrms` absent) | skip; the stored value is kept |
| `{"clear": true}` | erase the stored value for this leg |

**Request**
```json
{
  "cmd":   "cal_reply",
  "vrms":  <float> | null,  // null / absent = skip, keep what is stored
  "clear": <bool>           // optional, default false; true = erase this leg
}
```

`clear: true` wins over any `vrms` sent alongside it.

**Reply**
```json
{ "ok": true }
```

---

### `calibrate_spl`

Pistonphone-reference SPL calibration. Captures `capture_s` seconds
on the input channel after the user has applied a 94 dB SPL acoustic
reference, computes the captured RMS in dBFS, and stores that value
as `mic_sensitivity_dbfs_at_94db_spl` on the channel's cal entry. All
future dBFS readings on this channel can then convert to dB SPL via
`dbspl = dbfs - mic_sens_dbfs + 94.0`. Voltage-cal fields on the same
entry are preserved (`Calibration::load_or_new`).

**Request**
```json
{
  "cmd":            "calibrate_spl",
  "output_channel": <int>,        // optional, defaults to config
  "input_channel":  <int>,        // optional, defaults to config
  "capture_s":      <float>       // optional, default 1.0
}
```

**Reply**
```json
{ "ok": true }
```

Wire flow mirrors `calibrate`:

1. Daemon emits `cal_prompt` with `kind: "spl"` asking the user to
   seat the calibrator.
2. Client responds with `cal_reply` (any value — only the act of
   replying is meaningful).
3. Daemon captures, computes, saves.
4. Daemon emits `cal_done` then a terminal `done` / `error`.

**DATA — `cal_done`** (SPL flavour)
```json
// topic: cal_done
{
  "key":                              "out0_in0",
  "mic_sensitivity_dbfs_at_94db_spl": <float>,
  "kind":                             "spl",
  "error":                            "<message>"   // only on partial failure
}
```

---

### `calibrate_mic_curve`

Attach (or clear) a mic frequency-response correction curve on a
channel. The CLI parses the `.frd` / `.txt` file and uploads validated
arrays so the daemon never has to read user-supplied paths and the
flow works the same for local and remote daemons. Voltage and SPL
fields on the same entry stay untouched.

**Request — set**
```json
{
  "cmd":            "calibrate_mic_curve",
  "op":             "set",
  "output_channel": <int>,           // optional, defaults to config
  "input_channel":  <int>,           // optional, defaults to config
  "freqs_hz":       [<float>, ...],  // strictly increasing, 16–4096 entries
  "gain_db":        [<float>, ...],  // mic over-reads by this much; subtracted on read
  "source_path":    "<string>"       // optional, informational only
}
```

**Request — clear**
```json
{
  "cmd":            "calibrate_mic_curve",
  "op":             "clear",
  "output_channel": <int>,
  "input_channel":  <int>
}
```

**Reply**
```json
{ "ok": true, "key": "out0_in0", "loaded": <int> }   // 0 after clear
```

Validation: `freqs_hz` must be strictly increasing and finite, length
in `[16, 4096]`; `gain_db` must match length and be finite. Failures
return `{ "ok": false, "error": "<reason>" }` and leave the prior
curve (if any) untouched.

---

### `set_mic_correction_enabled`

Process-wide gate for daemon-side mic-curve application. Per-channel
curves stay loaded — this just controls whether they're applied to
emitted frames. Used by the UI's `Shift+M` keybinding for
diagnostics. Default on daemon start: `true`.

**Request**
```json
{ "cmd": "set_mic_correction_enabled", "enabled": <bool> }
```

**Reply**
```json
{ "ok": true, "enabled": <bool> }
```

While enabled, every monitor frame's `mic_correction` field reads
`"on"` (curve loaded and applied) or `"none"` (no curve on that
channel). When disabled, channels with a loaded curve report `"off"`
and emit raw uncorrected magnitudes; channels without a curve still
report `"none"`.

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
  "workers":       ["<cmd-name>", ...],
  "home":          "<path>",          // same identity fields as `status` — #385
  "config_path":   "<path>",
  "pid":           <int>,
  "started_at":    "<RFC3339>",
  "spawn_mode":    "auto" | "manual"
}
```

---

### `transfer_stream`

Streaming H1 transfer function estimator. Captures the selected measurement
+ reference input channels and publishes a new `transfer_stream` frame per
pair per iteration until stopped.

**Two different durations, and they are not the same number.** The H1 estimate
is taken over a sliding window of
`ac_core::visualize::transfer::capture_duration(4, sr)` — ≈ 2.5 s at 48 kHz —
but a *new frame is published every loop tick*, which is the 0.05 s drain
(`chunk_secs`) plus that tick's processing. **Measured 16.6 frames/s per pair**
on `--fake-audio` at 48 kHz over 30 s (median inter-frame gap 60.3 ms), and
17.5–18 frames/s per pair on the rig at 96 kHz. Earlier revisions of this
paragraph gave the 2.5 s window as the frame interval; that was wrong by a
factor of ~40 and was carried into two rig sessions as though it were the
contract. Consumers must not assume a frame rate — read the clock.

Runs in the `TRANSFER` concurrency group:
only one `transfer_stream` at a time, but coexists with `monitor_spectrum`
(`INPUT`) and any `OUTPUT` worker (each owns its own JACK client).

By default the daemon is **passive** — it does not drive any output. The
caller is expected to feed stimulus (pink noise, music, sweep, speech, …) into
the DUT externally and the daemon just observes `ref` → `meas`. Pass
`drive=true` to restore the self-stimulating pink-noise loopback mode.

Output-port **allocation** and **emission** are separate concerns:

| params | output ports | emits at launch |
|--------|--------------|-----------------|
| neither | none opened — daemon never touches the playback side | no |
| `drivable=true` | opened + connected at launch | no — gated on `set_drive` |
| `drive=true` | opened + connected (implies drivable) | yes, at `level_dbfs` |

`drivable` is what an interactive client (`ac transfer` / `ac-view`) wants: the
session comes up silent, but a later `set_drive` reaches the interface. Without
it the generator would play onto an unconnected port — audible silence with
every other observable looking correct.

**Request**
```json
{
  "cmd":          "transfer_stream",
  "drive":        <bool>,    // optional, default false — if true, daemon plays pink noise on the output
  "drivable":     <bool>,    // optional, default false — connect output ports at launch but stay
                             //   silent until `set_drive`. Implied by drive=true.
  "level_dbfs":   <float>,   // only meaningful when drive=true, default -10; clamped to
                             //   drive_max_dbfs (#360) before it seeds the drive state

  // Either the multi-pair form …
  "pairs":        [[<meas0>, <ref0>], [<meas1>, <ref1>], ...],

  // … or the single-pair legacy form (still accepted):
  "meas_channel": <int>,     // capture port index for the measurement signal (DUT output)
  "ref_channel":  <int>,     // capture port index for the reference signal (DUT input)

  // SPL session params (handoff: transfer-frame-v2 M0) — per-meas-channel,
  // static for the session (D10): set once here, not live-toggleable.
  "weighting":    "A" | "C" | "Z",     // optional, default "Z". Case-insensitive.
                                        // Strict 3-way — "off" is rejected
                                        // (unlike `set_band_weighting`'s 4-way enum).
  "integration":  "fast" | "slow"      // optional, default "fast". Case-insensitive.
                                        // "leq" is not implemented in M0 — rejected.
}
```

When `pairs` is present the daemon captures every unique channel referenced
in the list once per iteration and emits one `transfer_stream` DATA frame
per pair (each tagged with its own `meas_channel` / `ref_channel`). The
legacy single-pair form is equivalent to `pairs: [[meas_channel, ref_channel]]`.
`pairs` must be non-empty and every channel index must be within range.
`weighting`/`integration` apply to every pair in the session; invalid values
reply `{"ok": false, "error": "..."}` before the worker spawns.

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
  "type":            "transfer_stream",
  "cmd":             "transfer_stream",
  "freqs":           [<float>, ...],     // up to 2000 points
  "magnitude_db":    [<float>, ...],
  "phase_deg":       [<float>, ...],
  "coherence":       [<float>, ...],
  "re":              [<float>, ...],     // docs/superseded/unified.md Phase 3: complex H, real part
  "im":              [<float>, ...],     // complex H, imaginary part
  "delay_samples":   <int>,
  "delay_ms":        <float>,

  // Additive (#227) — whether `delay_samples` is a measured lock.
  "delay_locked":    <bool>,             // false while the pair is still warming
                                          // up, and when the estimator refused to
                                          // lock (no sufficiently prominent
                                          // cross-correlation peak: unpatched
                                          // reference, dead mic, or two inputs
                                          // carrying unrelated sources).
                                          //
                                          // `delay_samples` is never negative on an
                                          // accepted lock: the mic cannot lead the
                                          // electrical reference, so the estimator
                                          // selects over non-negative lags only. A
                                          // peak outside that range is reported —
                                          // `noncausal_peak_lag` below — but never
                                          // selected, and it does not by itself
                                          // cause a refusal.
                                          //
                                          // When false, `delay_samples` is 0 and
                                          // the pair is measured UNALIGNED — the
                                          // frame is still valid H₁, just not
                                          // delay-compensated. Consumers must not
                                          // read `delay_ms == 0.0` as "no delay":
                                          // a digital loopback legitimately reads
                                          // 0.0 (#216), so this flag is the only
                                          // thing separating the two.

  // Additive (#238) — how many delay estimates this pair has completed,
  // accepted or refused. 0 before the first attempt, and absent entirely on
  // a daemon predating #238 (consumers must default it to 0, never to "it
  // ran": absence is not evidence that the estimator answered).
  //
  // 0 is not observable from a #238 daemon: the same full-ring condition
  // gates the first estimate and the first published frame, so every frame a
  // subscriber receives already carries >= 1 (asserted in it_protocol.rs).
  // It is the default for the field's absence, not a state to build a warmup
  // display around.
  "delay_attempts":  <int>,              // the estimator has answered N times
                                          //
                                          // This is what separates warming up
                                          // from refusing. `delay_locked` is
                                          // false for both, so the flag alone
                                          // cannot carry it, and a fault
                                          // indicator that paints on warmup
                                          // gets ignored (#228, #238).
                                          //
                                          // The first attempt runs only once
                                          // the rings hold a full Welch
                                          // segment, so a clock started from
                                          // this field starts from the first
                                          // moment a lock was possible — not
                                          // from session start.
                                          //
                                          // MONOTONE FOR THE LIFE OF THE
                                          // PAIR. A re-lock (#226) adds
                                          // attempts and must never reset the
                                          // count: a consumer that reads
                                          // "has the estimator answered" from
                                          // it would see a locked-then-
                                          // refusing pair fall back to
                                          // "warming up", which is silence on
                                          // the fault indicator — the blank
                                          // window #238 removed, reappearing
                                          // only in the sessions #226 is for.
                                          //
                                          // WHY THIS IS NOT `delay_evidence`
                                          // BY ANOTHER NAME. The rule below
                                          // forbids gating on the evidence,
                                          // including reading its
                                          // null-versus-present to infer that
                                          // an attempt happened. That rule
                                          // protects the estimator's
                                          // thresholds and its choice of what
                                          // to publish. A count carries no
                                          // threshold: it says the estimator
                                          // answered, not how close the answer
                                          // came. The estimator may change
                                          // NOISE_FLOOR_PROMINENCE, its search
                                          // range, its peak rule, or refuse
                                          // for a reason not yet invented, and
                                          // every consumer of this field stays
                                          // correct. Anything that wants to
                                          // know *why* it refused is gating,
                                          // and belongs on the estimator's
                                          // side of the line.

  // Additive (#227) — the evidence the lock decision was made on, present
  // whether the estimate was accepted or refused. `null` before the first
  // attempt. DIAGNOSTIC ONLY: nothing downstream may gate on any of it, the
  // thresholds are the estimator's to own.
  //
  // This exists so the estimator's thresholds can be set from recorded
  // captures instead of another physical rig session (handoff-rig-session-2
  // Run C). A refusal's evidence is the valuable case — it is what separates
  // "move the microphone" from "the threshold is wrong".
  //
  // REPEATED EVERY FRAME even though it only changes when a lock is
  // attempted. That is deliberate, not an oversight to optimise away: DATA
  // is a PUB socket, the lock happens once at warmup, and a subscriber that
  // attaches a second later would never see a once-published value. A
  // capture script or a reconnecting viewer must get the evidence from the
  // next frame it receives. Measured cost is 1220 bytes against a ~190 kB
  // frame — 0.64%, against six 2000-point float arrays that dominate it.
  "delay_evidence": {
    "prominence":   <float>,             // peak_value / median_value. Accepted at
                                          // >= 24. Sets NOISE_FLOOR_PROMINENCE.
    "peak_lag":     <int>,               // lag of the strongest CAUSAL peak — what a
                                          // global-maximum rule would return over the
                                          // range the estimator may select from.
                                          // Differs from `delay_samples` exactly
                                          // when earliest-peak moved the estimate
                                          // off a reflection. Every threshold is
                                          // measured against this.
    "peak_value":   <float>,             // |rho| at peak_lag
    "noncausal_peak_lag":   <int>,       // lag of the strongest peak among the
    "noncausal_peak_value": <float>,     // NEGATIVE lags, and |rho| there. Nothing
                                          // physical arrives before the reference
                                          // carries it, so this is never selected
                                          // and no threshold is measured against it.
                                          // It is published because a peak there is
                                          // a real observation: #216's ring skew put
                                          // every session 0.2 s negative, and a
                                          // stimulus onset inside the correlation
                                          // window throws ripples across the lag
                                          // range. Both show as this standing clear
                                          // of `negative_lag_median`. Exceeding
                                          // `peak_value` is NOT a refusal on its own
                                          // — rig session 2's -826 ms lock had the
                                          // true arrival at +4.52 ms, still there
                                          // and still recoverable. 0.0 when the
                                          // search range holds no negative lags.
    "median_value": <float>,             // median |rho| over the searched lags
    "negative_lag_median": <float>,      // median |rho| over the NEGATIVE lags only,
                                          // where a causal path puts no signal at
                                          // all. `median_value` is taken over every
                                          // lag, and on a reverberant path most lags
                                          // hold reverberation — so the floor
                                          // `prominence` divides by is contaminated
                                          // by the thing it discriminates against.
                                          // This one is not. Published so the next
                                          // session can re-threshold offline; it
                                          // decides nothing today. 0.0 when the
                                          // search range holds no negative lags.
    "candidates": [                      // local maxima within 12 dB of the peak,
      {"lag": <int>, "value": <float>}   // lag order, strongest 32 kept — PLUS
    ]                                    // `delay_samples`, `peak_lag` and
  },                                     // `noncausal_peak_lag`, always present
                                          // whatever their rank, so a list may hold
                                          // up to 35. Lags are unique: the extras are
                                          // inserted only when the rank cut dropped
                                          // them, so a replay may sum or histogram
                                          // the list without double-counting. Rank
                                          // alone dropped the
                                          // accepted arrival at 3 m, where it was
                                          // weaker than 32 peaks of the reverberant
                                          // cluster: the capture then could not
                                          // reproduce the daemon's own decision,
                                          // which is the whole point of recording it.
                                          // 12 dB is wider than the 6 dB the
                                          // estimator accepts, on purpose: the
                                          // arrivals that say whether 6 dB is too
                                          // generous are the ones it rejects.
                                          // Sets DIRECT_PEAK_FRACTION — prominence
                                          // alone cannot, since it says nothing
                                          // about where the direct arrival sits
                                          // relative to the reflection beating it.

  // Additive (handoff: field-transfer M4d, #183) — raw input peaks for
  // the transfer view's input-level meters.
  "meas_peak_dbfs":  <float> | null,     // 20*log10(max|sample|) over this frame's
                                          // capture blocks, taken BEFORE any
                                          // calibration, weighting, or aggregation.
                                          // `null` for digital silence (-inf, which
                                          // JSON cannot represent).
  "ref_peak_dbfs":   <float> | null,     // same, reference channel

  "meas_channel":    <int>,
  "ref_channel":     <int>,
  "sr":              <int>,
  "mic_correction":  "on" | "off" | "none",

  // Additive (handoff: transfer-frame-v2 M0) — per-channel calibrated
  // spectra + SPL scalar + processing tags, derived from the same
  // capture/Welch segments as H₁ above, so everything on screen shares
  // one time origin (D2).
  "spec_freqs":      [<float>, ...],     // log-spaced column centre frequencies,
                                          // 48 cols/octave, 20 Hz-Nyquist, K≈480 at
                                          // 48 kHz. Identical every frame in a session.
  "meas_spectrum":   [<float>, ...],     // LINEAR amplitude, band-power aggregated
                                          // (ac-core::visualize::aggregate::
                                          // spectrum_to_columns_wire) from the meas
                                          // channel's Welch amplitude spectrum.
                                          // Calibrated: voltage cal + mic curve
                                          // applied in linear domain. NOT dB.
  "ref_spectrum":    [<float>, ...],     // same, reference channel — no mic curve
                                          // (ref leg is guarded, see above), voltage
                                          // cal still applied if present.
  "spl":             <float> | null,     // per meas channel: A/C/Z-weighted,
                                          // fast/slow time-integrated, SPL-offset-
                                          // applied dB SPL scalar. null when the
                                          // meas channel has no SPL cal layer.
  "spl_weighting":   "A" | "C" | "Z",    // echoes the session's `weighting` param
  "spl_integration": "fast" | "slow",    // echoes the session's `integration` param
  "cal_tags": {                          // per-channel provenance (tier-framing
                                          // labelled-tag rules, #97/#98 vocabulary)
    "meas": {
      "voltage":   "on" | "none",        // vrms_at_0dbfs_in present and applied
      "spl":       "on" | "none",        // SPL cal layer present and applied
      "mic_curve": "on" | "off" | "none" // same vocabulary as `mic_correction`
    },
    "ref": {
      "voltage":   "on" | "none",
      "spl":       "on" | "none",
      "mic_curve": "none"                // always "none" — a ref-channel mic
                                          // curve is refused at request time
    }
  },
  "drive": {                             // observed stimulus state (#228)
    "on":         <bool>,                // what is applied to the engine on this
                                          // tick, after the dead-man and clamp
    "level_dbfs": <float> | null,        // applied (clamped) level; null while off
    "drivable":   <bool>                 // session opened output ports at launch
  }
}
```

**`drive` is observed, not commanded.** It is built from the values applied
to the engine on that tick — after the `set_drive` dead-man (1.5 s) has
expired a stale drive, and after the clamp to `drive_max_dbfs`. A client that
rendered its own last `set_drive` instead would show the drive live while the
daemon had already silenced it, or show the requested level while a lower one
was being emitted. `level_dbfs` is `null` while off so there is no stale
number to misread, and carries the applied value while on — a drive clamped
to something inaudible is a real measurement with a bad SNR, which is neither
"on" nor "off" as a fault.

`drivable` is `false` for a fully passive session (external-DUT workflow, no
output ports opened). `drivable: true, on: false` is a session that could
drive and is not — the difference matters to a consumer deciding whether
silence on the inputs is expected.

**Linear-amplitude contract.** `meas_spectrum` / `ref_spectrum` carry
**linear amplitude only** — the daemon never converts them to dB. dB
conversion happens in the receiver, nowhere else — the single such site
for these fields is `ac-scene` (from M2 onward). This mirrors the
existing `visualize/spectrum` frame's `spectrum` field and closes the
historical "dual trace" / N-dependence class of bug (#142/#162):
band-power aggregation (√Σamp²) is N-independent for both discrete
tones and broadband content, and summing dB values instead would not be.

**Wire cost.** Measured at K=491 columns (48 kHz session): the M0
fields add ≈31 KB/frame/pair (JSON text — the K≈480 f64×2-channel
estimate that motivated the fixed grid assumed binary encoding; this
wire is JSON text, roughly 4× that). `meas_spectrum`/`ref_spectrum`
(2×K f64 values as decimal text) dominate. If wire economy needs to
improve later, the designated first lever is **precision reduction**
(round to fewer significant digits before serializing, or ship as f32)
— not shrinking `K` or reverting to per-column dB, both of which would
undo D18's N-independence guarantee.

**Not an IEC 61672 sound level meter.** `spl` is a Welch/FFT band-power
sum, weighted (A/C/Z, IEC 61672-1 Annex E curve formulas) and time-
integrated (fast/slow, matching IEC 61672-1's F/S time constants) —
but it is not a IEC 61672-1-compliant true-RMS SLM measurement chain.
The weighting curves and time constants are standards-conformant; the
upstream level (an aggregated linear-amplitude spectrum, not a
continuously-integrated pressure-squared signal) is not. Same caveat
as the existing `fractional_octave_leq` frame — display-only, must not
be quoted as a certified SPL reading.

**Complex H consistency.** `re` and `im` carry H₁(ω) directly so Tier 2
views can render Nyquist locus, IFFT-based impulse response, and
group-delay-from-complex without re-deriving from `magnitude_db` /
`phase_deg`. All four representations (`mag`, `phase`, `re`, `im`) are
computed from the same `H₁ = G_xy_comp / G_xx` complex value and are
guaranteed mutually consistent: `magnitude_db = 20·log10(√(re² + im²))`
and `phase_deg = atan2(im, re)·180/π`. Mic-curve correction (when
enabled on the meas channel) is applied to all four — magnitude has
the dB correction subtracted; (re, im) are scaled by `10^(−curve_db/20)`
so `arg(H)` is unchanged. Older subscribers that ignore `re` / `im`
keep working — the fields are pure additions.

#### `visualize/ir` sidecar (Phase 4b)

Emitted alongside each `transfer_stream` frame for the same pair on
the same tick — daemon-side IFFT of the full-resolution H₁(ω) into
the time domain. Separate frame so subscribers that only want H(ω)
don't pay the per-tick IR bandwidth, and so the IR view can be
toggled on/off in the UI without re-issuing the transfer command.

```json
// topic: data
{
  "type":          "visualize/ir",
  "cmd":           "transfer_stream",
  "samples":       [<float>, ...],   // h(t) downsampled to ≤2000 samples
  "sr":            <int>,            // capture sample rate
  "stride":        <int>,            // downsample factor (ir_full / samples)
  "dt_ms":         <float>,          // ms per output sample (1000/sr * stride)
  "t_origin_ms":   <float>,          // negative — t=0 sits at samples.len()/2
  "ref_channel":   <int>,
  "meas_channel":  <int>,
  "delay_samples": <int>,
  "delay_ms":      <float>,
  "delay_locked":  <bool>            // #227 — see transfer_stream above. When
                                     // false the IR is UNALIGNED, so the peak
                                     // sits at the true path delay rather than
                                     // at t=0.
}
```

**Time origin.** `samples` is `fftshift`-centred so the dominant IR
peak sits at the middle of the array. The first sample's time is
`t_origin_ms` (negative); each subsequent sample is `dt_ms` later. Tap
`k` is at `t_origin_ms + k * dt_ms` ms. Pre-causal taps in the lower
half of the array are normal output of a delay-compensated H₁
estimate — they capture phase wrap and pre-ringing of bandlimited
filters, not actual non-causality.

**No mic-curve correction.** The IR is computed from the raw
ac-core `TransferResult.re` / `.im` (full resolution), which is NOT
mic-curve-corrected — only the downsampled re/im in the
`transfer_stream` frame is. For visualisation-only Tier 2 use this is
acceptable; if a calibrated IR is wanted, run the Tier 1 sweep
measurement instead.

**DATA** — terminal after `stop`:
```json
// topic: done
{ "cmd": "transfer_stream", "stopped": true }
```

**Why the peaks are raw.** `meas_peak_dbfs` / `ref_peak_dbfs` are
deliberately NOT taken from `meas_spectrum` / `ref_spectrum` or from the
window-normalised amplitudes behind them. The meters these feed exist to
judge gain staging, and a calibrated or band-aggregated value hides
clipping — which is the one thing an input meter must never do. A
voltage-calibrated session and an uncalibrated one publish the same peak
for the same physical input.

Consumers should treat an **absent** field and an explicit **null** as
the same thing (no reading), and must not use the field's presence to
detect daemon version: the display truth for "silent channel" and "old
daemon" is identical, so any branch on the difference is a second
behaviour with no observable justification.

---

### `set_drive`

Starts, stops, or re-levels the stimulus of a **running**
`transfer_stream` session (handoff: field-transfer §4.3, #183). Valid
only while such a session runs — the drive state lives inside that
session's worker, the same lifetime rule `snapshot` follows.

Like `snapshot`, this command targets a live worker rather than spawning
one, so it does not go through the busy guard. That is load-bearing
rather than incidental: routing it through the guard would make it
contend with the very worker it targets, and since this is also the
command that STOPS the drive, the contention would land on the
panic-stop path.

**CTRL**
```json
{ "cmd": "set_drive", "on": true | false, "level_dbfs": <float> }
```

`level_dbfs` is **required on every request, including `on: false`**.
Every message doubles as the keepalive (below), so every message is a
full state assertion rather than a delta against server-remembered
state.

**Reply** — echoes the **applied** state:
```json
{ "ok": true, "on": true, "level_dbfs": -10.0 }
```

`level_dbfs` in the reply is what was applied after the server-side
clamp to `drive_max_dbfs` (config, default `-10.0`), which may differ
from what was requested. **A clamp is success, not a partial failure**:
a stimulus command that errors instead of applying a safe level is a
worse failure in the field than one that quietly applies the ceiling.
There is no `clamped` flag — the client knows what it asked for, and a
second field that must agree with the first is a second thing to keep
consistent. The client is expected to clamp too; this one is
authoritative.

A missing `on`, or a missing or non-finite `level_dbfs`, is a request
error (`{"ok": false, "error": ...}`) rather than something to coerce —
this is the one command where a silently substituted number reaches a
loudspeaker.

**Errors**
```json
{ "ok": false, "error": "no transfer_stream session running" }
```

**Keepalive / dead-man.** There is no separate keepalive command and no
sequence number. Every `set_drive` refreshes the session's keepalive
timestamp; the client re-sends its current state every 250 ms while
driving, and a byte-identical resend is the normal case — indistinguish-
able from any other request, and cheap (the worker acts only on observed
transitions, so an unchanged resend never re-drives the engine). If more
than **1500 ms** passes with no `set_drive`, the worker drops drive on
its own; **the session keeps running**. 250 ms against 1500 ms is a 6x
margin — do not change either without changing both.

The timestamp is monotonic, not wall-clock: a clock step must not be
able to extend or trip a dead-man whose purpose is stopping an
unattended process from driving a loudspeaker.

The dead-man arms on the **first `set_drive`** received by the worker and
remains armed for the rest of the session — not at session start. A
session launched with the legacy `drive: true` param and driven by a
script sends no keepalives; arming at start would silence it after 1.5 s.
That launch-time path retains its existing **unsupervised** semantics
(it drives until stopped, on the operator's deliberate responsibility);
the dead-man governs only stimulus started through `set_drive`. Every
UI-driven session goes through `set_drive`, so the UI is always covered.

**Sessions always launch with drive off** unless the legacy launch-time
`drive` param says otherwise. `ac transfer` never sets it.

An `on: false → true` transition discards a per-pair delay lock **that
was acquired while the drive was off**, flushing that pair's averages and
re-settling (2.56 s at the bottom stage). A lock acquired while driving is
kept, so the dead-man expiring and the client resuming does not disturb a
running measurement. `on: true → false`, a level change, and a repeated
`on: true` keepalive never discard a lock. `delay_attempts` counts the
re-estimate and never resets. See `relock`, below, for the manual half of
this (#226).

---

### `relock`

Discards every pair's held delay lock in the **running** `transfer_stream`
session (#226), so each pair's next tick retries acquisition from
scratch. A held lock is a maintained quantity, not a cached one: this is
the operator half of what invalidates it — the other half is the drive
off→on transition documented under `set_drive`, above, and both go
through the same flush.

Dispatched like `set_drive`/`snapshot`: targets a live worker rather than
spawning one, so it does not go through the busy guard.

**CTRL**
```json
{ "cmd": "relock" }
```

No arguments — session-wide, not per-pair. A per-pair variant is scope
this command does not need.

**Reply**
```json
{ "ok": true }
```

**Errors**
```json
{ "ok": false, "error": "no transfer_stream session running" }
```

**What it does not touch.** `delay_attempts` keeps counting and never
resets — a re-lock is another attempt, not evidence the pair was never
asked. `delay_evidence` (the last attempt's prominence/peak data) is
overwritten by the next attempt, not cleared by the request itself.

---

### `snapshot`

Freezes the active `transfer_stream` session's raw capture ring into a
self-contained `.acsnap` file (handoff: snapshot-backend M1). Valid only
while a `transfer_stream` session is running — the ring lives inside that
session's worker. See `ac-rs/SNAPSHOT.md` for the full `.acsnap` binary schema.

Ungated by the busy guard (below) — reads shared state and writes a spool
file, doesn't spawn a worker or touch audio I/O, so it runs regardless of
what else is active (same as `get_calibration`/`status`/`devices`).

**Request**
```json
{ "cmd": "snapshot" }
```

**Reply — success**
```json
{
  "ok":          true,
  "id":          "<sha256 of the .acsnap file — also its fetch handle>",
  "bytes":       <int>,
  "duration_s":  <float>,
  "channels":    ["meas_0", "ref", ...],
  "sha256":      "<same as id>"
}
```

`id` is the file's own sha256 — content-addressed, so identical
snapshots share one spool entry. **No daemon filesystem path is ever
returned** (D6) — remote is first-class; a client fetches by `id` only,
never by path.

**Reply — no session running**
```json
{ "ok": false, "error": "no transfer_stream session running" }
```

---

### `snapshot_fetch`

Chunked read of a spooled `.acsnap` over CTRL (DATA stays pure frames,
per D6). Client reassembles chunks by `offset` and verifies the whole
file against `sha256` from the `snapshot` reply.

**Request**
```json
{
  "cmd":    "snapshot_fetch",
  "id":     "<id from the snapshot reply>",
  "offset": <int>,           // byte offset, default 0
  "len":    <int>            // bytes requested, default/max 262144 (256 KiB)
}
```

`len` is silently clamped to 262144 — chosen for CTRL sanity (a single
REQ/REP round-trip stays fast even over a slow remote link). Larger
requests are not an error; they're served in multiple chunks.

**Reply**
```json
{
  "ok":          true,
  "id":          "<id>",
  "offset":      <int>,
  "chunk_b64":   "<base64-encoded chunk bytes>",
  "chunk_len":   <int>,       // decoded byte length of chunk_b64
  "total_bytes": <int>        // total file size — client stops when offset+chunk_len >= total_bytes
}
```

**Reply — unknown id**
```json
{ "ok": false, "error": "unknown snapshot id '<id>'" }
```

---

### `snapshot_list`

Lists spooled snapshots from the current session.

**Request**
```json
{ "cmd": "snapshot_list" }
```

**Reply**
```json
{
  "ok": true,
  "snapshots": [
    { "id": "<id>", "bytes": <int>, "duration_s": <float>, "channels": [...], "sha256": "<id>" }
  ]
}
```

---

### `snapshot_delete`

Removes one spooled snapshot before session end (session end also clears
the whole spool — see retention policy below).

**Request**
```json
{ "cmd": "snapshot_delete", "id": "<id>" }
```

**Reply**
```json
{ "ok": true }
{ "ok": false, "error": "unknown snapshot id '<id>'" }
```

---

**Snapshot retention policy.** The spool is cleared when its
`transfer_stream` session's worker stops — a snapshot is only valid
while its session runs (deliverable 2), so every `.acsnap` taken during a
session is deleted when that session ends. As a crash-safety fallback (a
killed daemon skips its own cleanup), the spool directory is also wiped
at the *start* of every new `transfer_stream` session, so a stale file
from a prior crashed session never outlives the next session's start.
Spool location: `~/.config/ac/snapshots/` by default, overridable via
`setup`'s `snapshot_spool_dir` (or `snapshot_ring_s` for the ring's
retention window, default 30 s) — never exposed in any CTRL reply.

---

### `probe`

Discovers routing: which playback ports carry an analog signal, and which
capture ports are looped back to them. Spawns a worker and reports on
DATA; the CTRL reply only confirms the launch.

**The worker emits a 1 kHz tone at −10 dBFS**, on one playback port at a
time, with every other output disconnected. Nothing else on the wire
announces this, so a caller driving unattended hardware is responsible
for the level at the far end.

The output scan needs a DMM (`dmm_host` in config). Without one it is
skipped entirely and **every** playback port is treated as analog for the
loopback phase, which is a weaker claim than a measured one — the
`output_skip` frame is the only signal that this happened.

**Request**
```json
{ "cmd": "probe" }
```

No arguments.

**Reply**
```json
{ "ok": true, "n_playback": <int>, "n_capture": <int> }
```

**DATA frames.** Every frame carries `"cmd": "probe"` and a `"phase"`:

| `phase` | fields | when |
|---|---|---|
| `output_start` | `n_ports` | DMM configured; opens the output scan |
| `output` | `channel`, `port`, `vrms`, `analog` | once per playback port |
| `output_skip` | `message` | no `dmm_host`; replaces the two rows above |
| `loopback_start` | `n_outputs`, `n_inputs` | opens the loopback scan |
| `loopback` | `out_ch`, `out_port`, `in_ch`, `in_port`, `level_dbfs` | a pair that carried signal |

`vrms` is `<float> | null` — null when the DMM read failed, which is not
the same as a measured zero. `analog` is `vrms > 10 mVrms`.

`loopback` is emitted **only** for a pair measuring above −30 dBFS, so a
pair that never appears means no loopback was detected, not that a frame
was dropped. `level_dbfs` is rounded to one decimal.

**Terminal frame** (`done`):
```json
{ "cmd": "probe", "analog_channels": [<int>, ...] }
```

An `error` frame with `"message"` replaces the `done` when the backend
does not support port routing, when there are no playback ports, or when
the engine fails to start.

---

### `test_software`

Pure-software self-test of the analysis and conversion path: no audio
device, no worker, no DATA traffic. The reply is synchronous and carries
the whole result, so this is the one self-test a client can run without
subscribing to DATA.

**Request**
```json
{ "cmd": "test_software" }
```

**Reply**
```json
{
  "ok":       true,
  "results":  [
    { "name": "<string>", "pass": <bool>, "detail": "<string>" }
  ],
  "all_pass": <bool>
}
```

`all_pass` is the AND over `results[].pass`. `ok` is `true` whenever the
suite ran — **a failed check is `all_pass: false`, not `ok: false`.** A
client that only reads `ok` will call a failing daemon healthy.

`name` and `detail` are display strings; their wording is not part of the
contract. `results` order is the order the checks ran.

---

### `test_hardware`

Analog loopback self-tests of the interface itself: noise floor, level
linearity, THD floor, frequency response, channel match, repeatability.
With `dmm: true` and a `dmm_host` configured it also runs three
DMM-corroborated tests (absolute level, level tracking, frequency
response). Spawns a worker; results arrive on DATA.

Requires a reference channel — with neither `reference_channel` nor
`reference_port` set it refuses with
`{"ok": false, "error": "reference channel not configured — ..."}`.

**Request**
```json
{ "cmd": "test_hardware", "dmm": <bool> }
```

`dmm` is optional, default `false`. The DMM tests run only when it is
`true` **and** `dmm_host` is configured; asking for them without a DMM
configured is silently a no-op, visible only as `dmm_run: 0` in the
terminal frame.

**Reply**
```json
{
  "ok":           true,
  "out_port":     "<port>",
  "ref_out_port": "<port>",
  "in_port":      "<port>",
  "ref_port":     "<port>",
  "warnings":     ["<string>", ...]   // optional — see `warnings` above
}
```

**DATA frames** — one `test_result` per check:
```json
{
  "type":           "test_result",
  "cmd":            "test_hardware",
  "name":           "<string>",
  "pass":           <bool>,
  "detail":         "<string>",
  "tolerance":      "<string>",
  "mic_correction": "on" | "off" | "none",
  "spl_offset_db":  <float> | null
}
```

`mic_correction` and `spl_offset_db` use the same vocabulary as the
monitor frames. The DMM-corroborated results carry `"dmm": true` and
**omit** `mic_correction` and `spl_offset_db` — a parser that requires
those two fields will fail on the DMM rows.

**Terminal frame** (`done`):
```json
{
  "cmd":        "test_hardware",
  "tests_run":  <int>, "tests_pass": <int>,
  "dmm_run":    <int>, "dmm_pass":   <int>,
  "xruns":      <int>
}
```

A `stop` cuts the suite between tests, so `tests_run` is the number that
actually ran, not the size of the suite.

---

### `test_dut`

Qualification suite for a device under test: noise floor, gain, THD vs
level, frequency response, clipping point. With `compare: true` it runs
the same five again with the DUT bypassed, so the client can report
deltas. Spawns a worker; results arrive on DATA.

Requires a reference channel, and refuses with the same error as
`test_hardware` when none is configured.

**Request**
```json
{ "cmd": "test_dut", "compare": <bool>, "level_dbfs": <float> }
```

`compare` optional, default `false`. `level_dbfs` optional, default
`-20.0` — it sets the drive for the gain and frequency-response tests.

**Reply** — same shape as `test_hardware`:
```json
{
  "ok":           true,
  "out_port":     "<port>",
  "ref_out_port": "<port>",
  "in_port":      "<port>",
  "ref_port":     "<port>",
  "warnings":     ["<string>", ...]   // optional
}
```

**DATA frames** — `test_result`, as in `test_hardware` but with a `tag`:
```json
{
  "type":           "test_result",
  "cmd":            "test_dut",
  "tag":            "dut" | "bypass",
  "name":           "<string>",
  "pass":           <bool>,
  "detail":         "<string>",
  "tolerance":      "<string>",
  "mic_correction": "on" | "off" | "none",
  "spl_offset_db":  <float> | null
}
```

**Compare mode.** After the `dut` pass the worker publishes a prompt and
waits for `dut_reply`:
```json
{ "type": "dut_compare_prompt", "cmd": "test_dut",
  "message": "Bypass DUT and press Enter" }
```

The wait has a **300 s deadline, and expiry is not an error**: the bypass
pass runs anyway, and its frames are tagged `"bypass"` whether or not the
operator actually bypassed anything. Nothing on the wire distinguishes an
answered prompt from a timed-out one, so **a `bypass` tag is not evidence
that the DUT was bypassed** — a client that needs that distinction must
track whether it sent `dut_reply` itself.

**Terminal frame** (`done`):
```json
{ "cmd": "test_dut", "tests_run": <int>, "compare": <bool>, "xruns": <int> }
```

---

### `dut_reply`

Answers the `dut_compare_prompt` published by a `test_dut` running in
compare mode. Like `set_drive` and `snapshot` it targets a live worker
rather than spawning one, so it does not go through the busy guard.

**Request**
```json
{ "cmd": "dut_reply" }
```

**Reply**
```json
{ "ok": true }
```

The reply is **always** `{"ok": true}`, including when no `test_dut` is
waiting for it. The send is best-effort into the waiting worker's
channel; the reply says the command was accepted, never that a prompt was
answered. A client cannot use it to discover whether a suite is running.

---
## Busy guard

Audio commands are classified into four concurrency groups:

| Group | Commands |
|-------|---------|
| `OUTPUT`    | `sweep_level`, `sweep_frequency`, `generate`, `generate_pink` |
| `INPUT`     | `monitor_spectrum` |
| `TRANSFER`  | `transfer_stream` |
| `EXCLUSIVE` | `plot`, `plot_level`, `plot_ir`, `calibrate`, `probe`, `test_hardware`, `test_dut` |

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

### Unparseable config.json (#370)
```json
{ "ok": false, "error": "config.json: <parse error>" }
```
`config.json` is reloaded from disk before every CTRL request (see
"Config reload" below). If the reload fails — bad JSON, or a file caught
mid-write — routing-affecting commands (anything that resolves a port from
config: `calibrate`, `calibrate_spl`, `plot`, `plot_level`, `plot_ir`,
`generate`, `generate_pink`, `sweep_level`, `sweep_frequency`,
`monitor_spectrum`, `transfer_stream`, `test_hardware`, `test_dut`) refuse
with this shape instead of silently serving the last-known-good in-memory
config. Non-routing commands (`status`, `quit`, `devices`, …) are
unaffected, so the daemon stays reachable to diagnose the broken file. The
error clears itself on the next reload after the file is fixed — no
restart needed.

---

## Config reload

Before every CTRL request `dispatch()` re-reads `config.json` from disk and
replaces the in-memory `Config` (`server.rs`). This closes the gap where an
auto-spawned daemon outlives the `ac` command that spawned it: a config
edit made between two `ac` invocations reaches the very next command
against that daemon, not only a freshly-spawned process (#370). Routing
fields (`*_channel`, `*_port`) are already re-resolved per request via
`resolve_input`/`resolve_output`, so this completes that design rather than
adding a competing cache. See "Unparseable config.json" above for the
failure mode.

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
