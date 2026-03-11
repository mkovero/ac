# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This is `thd_tool` — a Python CLI for audio bench measurements (THD, THD+N, level sweeps, frequency sweeps) using a JACK audio backend.

## Install

```bash
pip install -e .
```

This installs two entry points:
- `ac` — the main CLI (`thd_tool/ac.py`)
- `thd` — legacy CLI (`thd_tool/cli.py`, kept for backward compat)

Also runnable as `python -m thd_tool`.

## Usage (quick reference)

JACK must be running before any measurement command:
```bash
jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2
```

```bash
ac devices                              # list JACK ports
ac setup output 11 input 0             # save port config
ac calibrate 1khz                      # interactive calibration
ac sweep level -20dbu 6dbu 1khz       # level sweep
ac sweep frequency 20hz 20khz 0dbu    # freq sweep
ac monitor thd 0dbu 1khz              # live THD monitor
ac generate sine 0dbu 1khz            # play tone
ac s f 20hz 20khz 0dbu show           # abbreviated + open plot
```

All args are positional and unit-tagged (no `--flags`). Abbreviations: `sweep`→`s`, `monitor`→`m`, `generate`→`g`, `calibrate`→`c`, `level`→`l`, `frequency`→`f`, `thd`→`t`, `sine`→`si`.

## Architecture

### Entry flow

`parse.py` → `ac.py` → `jack_measure.py` / `jack_calibration.py`

1. **`parse.py`** — token-based CLI parser. Each CLI token is classified by unit suffix (Hz, kHz, dBu, dBFS, Vrms, mVrms, Vpp, dB, s, ppd). Returns a plain dict describing the command. Designed to translate to C++ later.

2. **`ac.py`** — dispatch table maps command names to handler functions. Handlers convert between unit systems (dBu ↔ dBFS ↔ Vrms) using `conversions.py`, load calibration, call measurement functions, then save CSV + PNG.

3. **`audio.py`** — `JackEngine` class: wraps the `jack` Python library. Manages output ports (one JACK port per hardware channel), one input port, a ring buffer for capture, and a process callback for real-time output of a looped sine buffer. `find_ports()` enumerates physical JACK ports.

4. **`jack_measure.py`** — `jack_sweep_level()`, `jack_sweep_frequency()`, `jack_monitor()`. Each creates a `JackEngine`, iterates over the sweep points, calls `analyze()`, prints a table, and returns a list of result dicts.

5. **`analysis.py`** — `analyze(recording, sr, fundamental)`. FFT-based: computes spectrum with Hann window, finds fundamental and harmonics by peak search, returns THD%, THD+N%, noise floor, clipping flag, ac_coupled flag.

6. **`jack_calibration.py`** — `Calibration` class (load/save keyed by `out{N}_in{M}_{freq}hz` in `~/.config/thd_tool/cal.json`) + interactive `run_calibration_jack()` procedure (play tone → user reads DMM → loopback capture → derive input scaling).

7. **`config.py`** — persistent hardware config at `~/.config/thd_tool/config.json` (output_channel, input_channel, device, dbu_ref_vrms).

8. **`conversions.py`** — unit math: `vrms_to_dbu`, `dbu_to_vrms`, `dbfs_to_vrms`, format helpers.

9. **`plotting.py`** — matplotlib plots saved as PNG alongside CSV output.

10. **`io.py`** — `save_csv()` and `print_summary()`.

### Calibration model

Calibration stores `vrms_at_0dbfs_out` and `vrms_at_0dbfs_in` — the physical voltage corresponding to 0 dBFS full scale. All level conversions multiply/divide by these factors. Without calibration, levels are shown in dBFS only.

### Result dict keys (from `analyze()`)

`fundamental_hz`, `fundamental_dbfs`, `linear_rms`, `thd_pct`, `thdn_pct`, `harmonic_levels`, `noise_floor_dbfs`, `spectrum`, `freqs`, `clipping`, `ac_coupled`. Sweep functions add `drive_db`, `out_vrms`, `out_dbu`, `in_vrms`, `in_dbu`, `gain_db`.

## Dependencies

`jack` (python-jack / CFFI binding to libjack), `numpy`, `scipy`, `matplotlib`.

## Legacy / old code

`thd_tool/old/` and `thd_tool/old2/` are historical snapshots; ignore them.

---

## Room measurement scripts (OSM + Babyface)

These shell scripts live at the repo root and are independent of `thd_tool`. They wire up a RME Babyface (ALSA card 1) with OpenSoundMeter (OSM) over JACK for room/speaker measurements.

### Scripts

- **`osm-start.sh`** — sets CPU governor to `performance`, pins IRQs, forces PipeWire quantum/rate (48 kHz / 128 frames), launches OSM with real-time priority (`chrt -f 70`) pinned to cores 6–7, then restores `powersave` on exit.
- **`babyface.sh`** — main controller. Sources `functions.sh`, discovers JACK ports by name pattern, then dispatches:
  - `-c` / `-d` — connect / disconnect all (generator + reference + measurement)
  - `-g/-G` `-r/-R` `-m/-M` — connect/disconnect generator, reference, or measurement individually
  - `-x` — use XLR IN (INR / capture_AUX3) as reference instead of the default (REFL / capture_AUX2)
  - `-P` / `-p` — enable/disable 48 V phantom on AN1 mic input (with a confirmation prompt for `-P`)
  - `-i` — reset Babyface input gains and output mixer to known defaults (see below)
- **`functions.sh`** — sourced by `babyface.sh`; defines all the `Connect*`, `Disconnect*`, phantom, and gain functions. Sources `config.sh` for port variable definitions.
- **`config.sh`** — defines port name variables by grepping `jack_lsp` output (AUX0–AUX3 for inputs, AUX0–AUX3 for playback, plus OSM generator/reference/measurement ports).
- **`vol.sh`** — one-liner: sets Main-Out AN1, AN2, PH3, PH4 to the value passed as `$@` via `amixer`.

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
