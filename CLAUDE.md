# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This is `ac` — an audio bench measurement system (THD, THD+N, level sweeps, frequency sweeps, transfer functions). The full stack is implemented in Rust: CLI (`ac-cli`), daemon (`ac-daemon`), and GPU UI (`ac-ui`). Supports JACK and CPAL (PortAudio) audio backends.

When adding a new analysis feature, **first decide its tier** — Tier 1 (reference measurement, `ac-core/src/measurement/`) vs Tier 2 (live analysis, `ac-core/src/visualize/`). See `ARCHITECTURE.md`.

## Build

```bash
cd ac-rs && cargo build        # builds ac, ac-daemon, ac-ui
cargo test                     # 330 tests (ac-core 119, ac-cli 55, ac-daemon 43 + 11 it, ac-ui 102)
```

## Usage (quick reference)

Audio backend is auto-detected: JACK if available, otherwise CPAL. When using JACK, it must be running first:
```bash
jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2
```

```bash
ac devices                              # list audio ports
ac setup output 11 input 0             # save port config
ac calibrate                           # interactive calibration
ac sweep level -20dbu 6dbu 1khz       # level sweep
ac sweep frequency 20hz 20khz 0dbu    # freq sweep
ac monitor                             # live spectrum (default input)
ac monitor 0-3,5                       # live spectrum on channels 0–3 and 5
ac generate sine 0dbu 1khz            # play tone
ac s f 20hz 20khz 0dbu show           # abbreviated + open plot
```

All args are positional and unit-tagged (no `--flags`). Abbreviations: `sweep`→`s`, `monitor`→`m`, `generate`→`g`, `calibrate`→`c`, `level`→`l`, `frequency`→`f`, `thd`→`t`, `sine`→`si`.

## Package layout

```
ac-rs/                 (Rust — primary implementation)
  ZMQ.md               (wire protocol reference — authoritative)
  crates/
    ac-core/           (pure library: analysis, generator, calibration, config, IEC 61260-1 filterbank, Farina log-sweep IR, IEC 61672-1 A/C/Z weighting, AES17 idle-channel noise, HTML report renderer — 119 tests)
    ac-cli/            (CLI client: parser, ZMQ client, CSV export — 50 tests)
    ac-daemon/         (ZMQ REP+PUB server binary — 43 unit + 10 it tests)
    ac-ui/             (wgpu+egui GPU UI: spectrum, waterfall, CWT, transfer, sweep — 81 tests)

tests/                 (black-box pytest harness — spawns Rust daemon over ZMQ)
```

See `ac-rs/CLAUDE.md` and `ac-rs/ZMQ.md` for Rust crate docs.

## Daemon auto-spawn

The Rust CLI auto-spawns `ac-daemon` locally. Resolution order:
1. `ac-daemon` in `$PATH` (production install)
2. `ac-rs/target/debug/ac-daemon` (local dev build)

---

## Room measurement scripts (OSM + Babyface)

These shell scripts live in `scripts/` and are independent of `ac`. They wire up a RME Babyface (ALSA card 1) with OpenSoundMeter (OSM) over JACK for room/speaker measurements.

### Scripts

- **`scripts/osm-start.sh`** — sets CPU governor to `performance`, pins IRQs, forces PipeWire quantum/rate (48 kHz / 128 frames), launches OSM with real-time priority (`chrt -f 70`) pinned to cores 6–7, then restores `powersave` on exit.
- **`scripts/babyface.sh`** — main controller. Sources `functions.sh`, discovers JACK ports by name pattern, then dispatches:
  - `-c` / `-d` — connect / disconnect all (generator + reference + measurement)
  - `-g/-G` `-r/-R` `-m/-M` — connect/disconnect generator, reference, or measurement individually
  - `-x` — use XLR IN (INR / capture_AUX3) as reference instead of the default (REFL / capture_AUX2)
  - `-P` / `-p` — enable/disable 48 V phantom on AN1 mic input (with a confirmation prompt for `-P`)
  - `-i` — reset Babyface input gains and output mixer to known defaults (see below)
- **`scripts/functions.sh`** — sourced by `babyface.sh`; defines all the `Connect*`, `Disconnect*`, phantom, and gain functions. Sources `config.sh` for port variable definitions.
- **`scripts/config.sh`** — defines port name variables by grepping `jack_lsp` output (AUX0–AUX3 for inputs, AUX0–AUX3 for playback, plus OSM generator/reference/measurement ports).
- **`scripts/babyface-reset-vol.sh`** — one-liner: sets Main-Out AN1, AN2, PH3, PH4 to the value passed as `$@` via `amixer`.

### Port / signal routing

| Variable | JACK port | Physical |
|----------|-----------|----------|
| `INL` / `INR` | capture_AUX0/1 | XLR mic inputs AN1/AN2 |
| `REFL` / `REFR` | capture_AUX2/3 | Line inputs (reference mic) |
| `OUTL` / `OUTR` | playback_AUX0/1 | Main line outputs |
| `HEADL` / `HEADR` | playback_AUX2/3 | Headphone outputs |

`ConnectDefault` routes: OSM generator → all outputs (headphone + line), IN_L → OSM measurement, REFL or INR → OSM reference (depending on `-x` flag).

### `DefaultInputGain` resets

Sets Mic-AN1/AN2 gain to 0, Line-IN3/4 sensitivity to +4 dBu, Line-IN3/4 gain to 0, PAD off, and all Main-Out channels to 0 except AN1/AN2/PH3/PH4 which are set to 8192 (unity).

## Testing

Run all tests before committing:
```bash
cd ac-rs && cargo test
pytest tests/ -q                # black-box ZMQ protocol tests — default ~20 s
pytest tests/ --runslow -q      # + extended (`test_hardware`, `test_dut`) ~3 min
```

Two long pytest scenarios (`test_test_hardware_frames`, `test_test_dut_frames`)
are marked `slow` and skipped by default. Pass `--runslow` to include them, or
`pytest -m slow --runslow` to run *only* the extended suite.

## ds — diagnostics session manager

Companion tool to `ac`. Lives in `ds/`. Installed as the `ds` command via setup.py.

**Relationship to ac:**
- Reads `~/.config/ac/config.json` to get the active session name (`session` key)
- Reads `~/.local/share/ac/sessions/<name>/` for ac-produced files
- Never writes to ac config or session dirs outside its own `ds/` subdirectory
- No ZMQ, no dependency on ac internals

**Session directory layout:**
```
~/.local/share/ac/sessions/<name>/
  *.csv, *.png          # ac owns these
  ds/
    session.json        # device metadata, notes, file registry
    ai_log.json         # history of all AI calls
    files/              # scraped/fetched/added files, original formats
```

**Commands:**
```
ds status               # active session, file counts
ds ls                   # list ac files and ds files
ds note "<text>"        # add timestamped note
ds notes                # list all notes
ds add <path>           # add local file into session
ds rm <filename>        # remove file from session
ds fetch [query]        # scrape web for manuals/datasheets/forums
ds analyze              # full AI analysis of session
ds ask "<question>"     # ad-hoc AI query with session context
ds diff <a> <b>         # compare two sessions, AI interprets delta
ds log [--last N]       # show AI interaction history
```

**Requires:** ANTHROPIC_API_KEY env var for any AI commands.
