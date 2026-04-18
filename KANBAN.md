# Kanban — ac measurement system

> Maintained alongside GitHub Issues. Labels: `software` `hardware` `testing` `blocker` `phase-4`
> Last updated: 2026-04-17

---

## Todo

| # | Title | Area | Notes |
|---|-------|------|-------|
| - | Office visit: confirm transformer specs and relay inventory | `hardware` | **blocker** — Lundahl/Studer part numbers, relay coil voltage, Bosch panel connector types |

---

## In Progress

_nothing yet_

---

## Backlog

### Hardware

| Title | Depends on |
|-------|-----------|
| Calculate H-pad resistor values | office visit |
| Design input protection network | pad R values |
| Design INA134 differential receiver stage | protection network |
| Design OPA134 HiZ buffer stage (BNC probe path) | — |
| Design THAT1646 output stage | — |
| Design relay circuits (impedance, path selection, stereolink) | office visit (coil voltage) |
| Design PSU section | office visit (coil voltage) |
| KiCad schematic — analog frontend board (1/3) | all analog stages |
| KiCad schematic — digital backend board (2/3) | relay circuits |
| KiCad schematic — PSU board (3/3) | PSU design |
| Arduino Mega firmware | KiCad digital board |
| Make measurement cables | — |

### Testing

| Title | Notes |
|-------|-------|
| Add transfer command integration tests | after transfer command implemented |

---

## Done

| Title | Area | Landed |
|-------|------|--------|
| ac-ui sweep view: THD/THD+N, gain, spectrum panels — freq and level sweeps | `software` | 2026-04-17 |
| ac-cli: full Rust CLI port — 28+ commands, 50 parser tests | `software` | 2026-04-17 |
| Phase 4 — Rust daemon parity: calibrate, dmm_read, transfer, probe, test_hardware/dut, CPAL, GPIO | `software` | 2026-04-15 |
| ac-ui Phase 2: waterfall, GPU timing overlay, benchmark harness, per-cell views, multi-channel real-daemon, `ac ui` dispatch | `software` | 2026-04-15 |

---

## Reference

### Hardware design order (once office visit complete)
1. H-pad resistor values (600Ω confirmed or revised)
2. Input protection network (series R, zener/TVS selection)
3. INA134 application circuit
4. OPA134 HiZ buffer circuit
5. THAT1646 output stage
6. Relay circuits
7. PSU section
8. Mega firmware outline

### Key specs
- Max input: ~77Vrms (3kW/2Ω power amp), 109V peak
- Pad range: 0/20/40/60dB cumulative H-pad
- PSU: ±15V analog, 5V logic
- Relay driver: ULN2803 + flyback diodes
- Audio I/F: RME Fireface 400
- GPIO: Arduino Mega2560 (DIN-rail) via USB serial → ZMQ → ac daemon

### Software architecture quick ref
- Rust CLI: `ac-rs/crates/ac-cli` — positional parser + ZMQ REQ/SUB, 50 tests
- Rust daemon: `ac-rs/crates/ac-daemon` — ZMQ REP:5556 / PUB:5557, 43 tests
- Rust UI: `ac-rs/crates/ac-ui` — wgpu spectrum/waterfall/CWT, egui transfer/sweep, 54 tests
- DSP library: `ac-rs/crates/ac-core` — no sockets, 43 unit tests
- Python client: `ac/client/` — alternative CLI parser + ZMQ REQ/SUB
- Python GUI: `ac/ui/` — pyqtgraph, alternative to ac-ui
- Tests: `cargo test` (190 Rust) + `pytest tests/ -q` (156 Python)

*Updated: 2026-04-17*
