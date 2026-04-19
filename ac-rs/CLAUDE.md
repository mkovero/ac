# ac-rs — Rust audio measurement system

Full Rust implementation of the `ac` stack: CLI client, ZMQ daemon, and GPU UI.

## Build

```bash
cargo build                       # all crates
cargo build --release             # optimized
cargo test                        # 212 tests (ac-core 43, ac-cli 50, ac-daemon 43, ac-ui 76)
```

## Crate layout

| Crate | Binary | Role |
|-------|--------|------|
| `ac-core` | — | Pure library — analysis, CWT, generator, calibration, config, conversions. No sockets, no global state. 43 unit tests. |
| `ac-cli` | `ac` | CLI client — positional parser, ZMQ REQ/SUB, CSV export, daemon/UI auto-spawn. 50 parser tests. |
| `ac-daemon` | `ac-daemon` | ZMQ REP+PUB server. Audio I/O (JACK/CPAL/fake), worker management. Thin shell over `ac-core`. 43 tests. |
| `ac-ui` | `ac-ui` | GPU UI — wgpu spectrum/waterfall/CWT, egui transfer/sweep views. Connects via ZMQ SUB + REQ. 76 tests. |

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
| `L` | Cycle layout: grid → single → compare → transfer (sweep via --mode only) |
| `W` | Cycle view: spectrum → waterfall (FFT) → waterfall (CWT) → spectrum |
| `F` | Toggle fullscreen |
| `D` | Toggle timing overlay |
| `Tab` / `Shift+Tab` | Next/prev channel or grid page |
| `Space` | Toggle channel selection |
| `[` / `]` | Shift dB floor ±5 |
| `+` / `-` | Adjust dB span |
| `Left` / `Right` | FFT monitor tick interval ±1 ms (clamped 1–1000 ms, FFT mode only) |
| `Up` / `Down` | FFT monitor N (1024 … 65536, FFT mode only) |
| `Ctrl+R` | Reset all views |
| `S` | Screenshot |
| `P` | Toggle peak hold (Spectrum view) — fundamental + 2×–5× harmonic markers, auto-tracks the hottest peak |
| `Shift+Up/Down` | CWT sigma ±1 (5–24, only in CWT mode) |
| `Shift+Left/Right` | CWT scales ×2/÷2 (64–2048, only in CWT mode) |
| Scroll | Zoom freq/dB/time axis (context-dependent) |
| `Alt+Scroll` | Cycle waterfall palette (inferno → viridis → magma → plasma, Waterfall only) |
| Drag | Pan freq/dB axes |
| Right-click | Reset hovered cell view |

For the full backlog see <https://github.com/mkovero/ac/issues?q=is%3Aopen+label%3Abacklog>.
