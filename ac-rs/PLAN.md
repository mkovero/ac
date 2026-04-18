# ac-rs — Rust server

`ac/server/` has been fully replaced by the Rust daemon. The Python client, ZMQ wire protocol, and port numbers are unchanged. `ac-daemon` is the only server implementation.

---

## Goals

- Real-time safe JACK capture (no GIL, no surprise allocations in the audio callback)
- Library-first: core logic callable directly, network layer optional
- JSON at every module boundary — each crate can be tested without audio hardware
- Daemon mode for remote use (`ac server enable`), direct call for local use

## Non-goals (for now)

- ~~Rust CLI client~~ — **Done.** `ac-cli` crate, 28+ commands, 50 parser tests.
- ~~Plotting / UI~~ — **Done.** `ac-ui` crate: spectrum, waterfall, CWT, transfer, sweep views.
- DMM / SCPI — ported to Rust daemon (`dmm_read` handler)

---

## Wire protocol (unchanged)

| Socket | Address | Type | Purpose |
|--------|---------|------|---------|
| CTRL | `tcp://*:5556` | REP | JSON command → JSON reply |
| DATA | `tcp://*:5557` | PUB | `b"<topic> <json>"` frames |

**Topics:** `data`, `done`, `error`, `cal_prompt`, `cal_done`

**Commands to implement:**
`status`, `quit`, `stop`, `devices`, `setup`, `get_calibration`, `list_calibrations`,
`sweep_level`, `sweep_frequency`, `monitor_spectrum`, `generate`, `calibrate`, `cal_reply`, `dmm_read`

Result dict keys must match Python exactly — the Python client deserializes these fields by name.

---

## Crate structure

```
ac-rs/
  Cargo.toml          (workspace)
  PLAN.md             (this file)

  crates/
    ac-core/          # pure library — no sockets, no global state, 43 tests
      src/
        analysis.rs   # FFT, THD, THD+N (mirrors analysis.py)
        cwt.rs        # Morlet continuous wavelet transform (sparse freq-domain)
        generator.rs  # sine, sweep waveform generation
        calibration.rs# Calibration struct, load/save cal.json
        types.rs      # all shared JSON-serializable structs (serde)
        config.rs     # load ~/.config/ac/config.json
        conversions.rs# vrms_to_dbu, dbfs helpers
        constants.rs  # SAMPLERATE, FFT_WINDOW, NUM_HARMONICS, etc.

    ac-cli/           # CLI client — full port of ac/client/, 50 tests
      src/
        main.rs       # arg dispatch
        parse.rs      # positional token parser
        client.rs     # ZMQ REQ/SUB wrapper
        io.rs         # CSV export, print helpers
        spawn.rs      # daemon/UI auto-spawn
        commands/      # one file per command group

    ac-daemon/        # ZMQ REP+PUB wrapper around ac-core
      src/
        main.rs       # bind sockets, dispatch commands, spawn workers
        server.rs     # main loop, worker reaping
        handlers.rs   # command handlers
        audio/        # jack_backend, cpal_backend, fake

    ac-ui/            # GPU UI — wgpu + egui
      src/
        main.rs       # CLI args, window setup
        app.rs        # event loop, render dispatch
        data/         # ZMQ receiver, triple-buffer store, types
        render/       # spectrum (wgpu), waterfall (wgpu), grid/transfer/sweep (egui)
        ui/           # layout, overlay, export
```

### Why this split

`ac-core` has zero network code. You can call `analysis::analyze(&samples, sr, fundamental)` and get back an `AnalysisResult` struct directly. `ac-daemon` is the thin shell that serializes/deserializes JSON and owns the sockets.

This means:
- Unit tests feed raw sample buffers, get result structs — no sockets, no JACK needed
- Local use (future Rust CLI) calls `ac-core` directly
- Daemon use goes through `ac-daemon` — same `ac-core` underneath

---

## Key design decisions

### Audio thread → analysis thread

JACK callback must be real-time safe: no allocation, no lock contention, no system calls.
Use a lock-free ringbuffer (`ringbuf` crate) to move samples from the RT callback into an analysis worker thread.

```
JACK RT callback → ringbuf (wait-free write) → analysis thread → pub_queue channel → PUB socket
```

### Concurrency model (mirrors engine.py)

```rust
// engine.py categories:
// OUTPUT_CMDS  = { generate, sweep_level, sweep_frequency }
// INPUT_CMDS   = { monitor_spectrum }
// EXCLUSIVE    = { calibrate, ... }
// One audio worker at a time; CTRL socket serializes commands
```

Single `Arc<Mutex<Option<WorkerHandle>>>` guards the active measurement. CTRL loop stays single-threaded (ZMQ REP is inherently serial). Workers run in `std::thread::spawn`.

### Calibration

Same file format as Python: `~/.config/ac/cal.json`, key pattern `out{N}_in{M}`.
`Calibration` struct holds `vrms_at_0dbfs_out` and `vrms_at_0dbfs_in`.

### Stale server detection

Python client compares `_SRC_MTIME` of server source files. Rust binary can expose build timestamp in the `status` reply instead — client can be updated to compare that, or we just remove the stale-detection mechanism for the Rust server.

---

## Crates to use

| Purpose | Crate |
|---------|-------|
| JACK backend | `jack` |
| PortAudio fallback | `cpal` |
| Lock-free ringbuffer | `ringbuf` |
| FFT | `rustfft` |
| ZMQ | `zmq` (libzmq bindings) |
| JSON serialize | `serde` + `serde_json` |
| Config file | `serde_json` (same format as Python) |
| Error handling | `anyhow` |

---

## Implementation phases

### Phase 1 — ac-core skeleton ✓
- [x] Workspace + crates scaffold (`Cargo.toml`, empty `lib.rs`)
- [x] `constants.rs`, `conversions.rs`, `types.rs` (pure data, no IO)
- [x] `analysis.rs` — port `analyze()` from Python, unit test with known signal
- [x] `generator.rs` — sine generation, level in dBFS → amplitude

### Phase 2 — audio backend ✓
- [x] JACK client: open client, register ports, RT callback → ringbuffer (`audio/jack_backend.rs`)
- [x] Fake audio backend matching same interface (`audio/fake.rs`) — used by `--fake-audio` and tests
- [x] CPAL fallback (`audio/cpal_backend.rs`, issue #21)

### Phase 3 — ac-daemon ✓
- [x] ZMQ REP+PUB setup, main loop (`server.rs`)
- [x] Worker reaping via `JoinHandle::is_finished()` in main loop
- [x] `status`, `devices`, `setup`, `quit`, `stop` commands
- [x] `generate`, `generate_pink` commands
- [x] `sweep_level`, `sweep_frequency` commands
- [x] `plot`, `plot_level` (point-by-point sweep with `analyze()`)
- [x] `monitor_spectrum` command
- [x] `calibrate` / `cal_reply` (stub — no real DMM interaction yet)
- [x] `get_calibration`, `list_calibrations`
- [x] `server_enable`, `server_disable`, `server_connections`
- [x] Python client auto-spawns Rust daemon via `os.execv` (`ac/__main__.py`)
- [x] Python server (`ac/server/`) deleted; Rust daemon is the only implementation
- [x] 149 Python tests pass against Rust daemon (`--fake-audio`); 29 Rust unit tests pass

### Phase 4 — parity ✓
- [x] Interactive `calibrate` / `cal_reply` flow (real DMM prompt loop) — issue #14
- [x] `dmm_read` — SCPI socket client — issue #15
- [x] `transfer` / `probe` commands — issues #16/#17
- [x] `test_hardware` / `test_dut` commands — issues #18/#19
- [x] CPAL fallback audio backend — issue #21
- [x] GPIO handler port — issue #20

### Known limitations (active backlog)

- JACK process callback not real-time safe (#23) — Mutex + alloc in RT thread;
  fix is the `ringbuf` SPSC originally specified in "Key design decisions" above
- xrun counter never incremented (#24)
- Capture rings grow unbounded on output-only commands (#25)
- calibrate save errors only surface in cal_done frame (#26)
- CPAL backend silently no-ops port routing (#27)
- GPIO REQ socket wedged after first recv timeout (#28)
- handlers.rs is 1931 LOC; split planned (#29)
- JACK resolver helpers spawn fresh clients per call (#30)
- PUB socket default HWM silently drops frames under load (#31)
- No daemon-level integration tests (#32)

---

## What stays in Python

- `ac/client/` — alternative CLI (Rust `ac-cli` is the primary now)
- `ac/ui/` — pyqtgraph views (alternative to Rust `ac-ui`)
- `ds/` — diagnostics session manager, AI analysis
- `scripts/` — babyface/OSM shell scripts

---

## ac-ui — GPU spectrum/sweep/transfer monitor

`crates/ac-ui` is a standalone wgpu/winit/egui binary. Views:

- **Spectrum** — wgpu log-freq × dB pipeline, scales to 100+ channels at 60 fps
- **Waterfall** — wgpu scrolling spectrogram (FFT or CWT)
- **Transfer** — egui H1 magnitude/phase/coherence (3-panel)
- **Sweep** — egui THD/THD+N/gain/spectrum (3-panel freq, 2-panel level)

Layouts: grid, single, compare, transfer, sweep. Sweep entered via `--mode sweep_frequency|sweep_level`.

```
ac-ui                               # auto-discovers daemon, starts spectrum view
ac-ui --mode sweep_frequency        # sweep view (launched by ac plot ... show)
ac-ui --synthetic --channels 10     # benchmark mode
```

See `ac-rs/CLAUDE.md` for keybindings.
