# ac-rs — Rust server

Rust implementation of the `ac` ZMQ server. Drop-in replacement for `ac/server/`.
The Python client (`ac/client/ac.py`) speaks to it unchanged.

## Build

```bash
cargo build -p ac-daemon          # debug (auto-discovered by Python client)
cargo build -p ac-daemon --release
cargo test -p ac-core             # 29 unit tests, no hardware required
```

## Crate layout

| Crate | Role |
|-------|------|
| `ac-core` | Pure library — analysis, generator, calibration, config, conversions. No sockets, no global state. |
| `ac-daemon` | ZMQ REP+PUB server binary. Thin shell over `ac-core`. |

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

## Known gaps vs Python server

- `calibrate`: emits `cal_prompt` but does not wait for a real `cal_reply` from a DMM
- `dmm_read`: always returns "no DMM configured"
- `transfer`, `probe`, `test_hardware`, `test_dut`: not yet implemented
- GPIO handler: not yet ported
- CPAL/sounddevice fallback: not yet implemented (JACK only)
