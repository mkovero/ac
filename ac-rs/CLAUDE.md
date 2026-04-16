# ac-rs — Rust server

Rust implementation of the `ac` ZMQ server. Drop-in replacement for `ac/server/`.
The Python client (`ac/client/ac.py`) speaks to it unchanged.

## Build

```bash
cargo build -p ac-daemon          # debug (auto-discovered by Python client)
cargo build -p ac-daemon --release
cargo build -p ac-ui              # wgpu spectrum/waterfall/transfer UI
cargo test -p ac-core             # 37 unit tests, no hardware required
```

## Crate layout

| Crate | Role |
|-------|------|
| `ac-core` | Pure library — analysis, cwt, generator, calibration, config, conversions. No sockets, no global state. |
| `ac-daemon` | ZMQ REP+PUB server binary. Thin shell over `ac-core`. |
| `ac-ui` | wgpu live spectrum/waterfall/transfer monitor. Connects to `ac-daemon` via ZMQ SUB + REQ. |

## ac-daemon binary

```
ac-daemon [--local] [--fake-audio] [--ctrl-port N] [--data-port N]
```

| Flag | Default | Effect |
|------|---------|--------|
| `--local` | off | Bind to `127.0.0.1` only (auto-spawned with this flag) |
| `--fake-audio` | off | Use synthetic sine loopback instead of JACK |
| `--ctrl-port N` | 5556 | ZMQ REP port |
| `--data-port N` | 5557 | ZMQ PUB port |

## Audio backends (`ac-daemon/src/audio/`)

| File | When used |
|------|-----------|
| `jack_backend.rs` | Default (JACK must be running) |
| `fake.rs` | `--fake-audio` flag; returns clean sine so `analyze()` gets plausible output |

## Server loop (`ac-daemon/src/server.rs`)

Single-threaded ZMQ REP/PUB loop. Workers run in `std::thread::spawn`.
Main loop drains the `pub_tx` channel (worker → PUB socket) and reaps finished
workers via `JoinHandle::is_finished()` every 10 ms poll interval.

## Handlers (`ac-daemon/src/handlers.rs`)

One function per command. Each audio command (`generate`, `plot`, etc.) checks
the busy guard (`check_busy`), spawns a `WorkerHandle`, inserts it into the
shared `workers` map, and returns the CTRL reply immediately.

## Protocol reference

See `ZMQ.md` — authoritative for both Python and Rust implementations.

## Backend status

| Path | State |
|------|-------|
| `calibrate` | Full state machine: emits `cal_prompt`, blocks on `cal_reply`, writes cal.json via `Calibration::save()` |
| `dmm_read` | SCPI client wired (only used when `[dmm]` section is configured; otherwise `no DMM configured`) |
| GPIO handler | USB2GPIO (Arduino Mega) handler in `gpio.rs`, spawned by `--gpio <port>` |
| CPAL backend | Runs when JACK unavailable. **Note:** CPAL backend inherits the `AudioEngine` default no-op routing methods — commands that rely on port routing (`probe`, `transfer`, `test_hardware`, `test_dut`) currently behave incorrectly. See issue #27. |
| `--fake-audio` | Synthetic sine loopback; bypasses routing (see issue #34) |

## Known limitations

- JACK process callback is not real-time safe today (Mutex + alloc on every
  period). See issue #23 — fix in flight via `ringbuf` SPSC + atomic tone swap.
- `xruns()` counter is always 0 on both JACK and CPAL (issue #24).
- Capture rings grow unbounded on long output-only commands (issue #25).
- `handlers.rs` is 1931 LOC; slated for split into per-concern modules (#29).

## ac-ui keybindings

| Key | Action |
|-----|--------|
| `L` | Cycle layout: grid → overlay → single → compare → transfer |
| `W` | Cycle view: spectrum → waterfall (FFT) → waterfall (CWT) → spectrum |
| `F` | Toggle fullscreen |
| `D` | Toggle timing overlay |
| `Tab` / `Shift+Tab` | Next/prev channel or grid page |
| `Space` | Toggle channel selection |
| `[` / `]` | Shift dB floor ±5 |
| `+` / `-` | Adjust dB span |
| `Ctrl+R` | Reset all views |
| `P` | Screenshot |
| `Shift+Up/Down` | CWT sigma ±1 (5–24, only in CWT mode) |
| `Shift+Left/Right` | CWT scales ×2/÷2 (64–2048, only in CWT mode) |
| Scroll | Zoom freq/dB/time axis (context-dependent) |
| Drag | Pan freq/dB axes |
| Right-click | Reset hovered cell view |

For the full backlog see <https://github.com/mkovero/ac/issues?q=is%3Aopen+label%3Abacklog>.
