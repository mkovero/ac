# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This is `thd_tool` — a Python CLI for audio bench measurements (THD, THD+N, level sweeps, frequency sweeps). Supports JACK and sounddevice (PortAudio) audio backends with auto-detection.

## Install

```bash
pip install -e .
```

This installs two entry points:
- `ac` — the main CLI (`thd_tool/client/ac.py`)
- `thd` — legacy CLI (`thd_tool/cli.py`, kept for backward compat)

Also runnable as `python -m thd_tool`.

## Usage (quick reference)

Audio backend is auto-detected: JACK if available, otherwise sounddevice (PortAudio). Force via config `"backend": "jack"` or `"backend": "sounddevice"`. When using JACK, it must be running first:
```bash
jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2
```

```bash
ac devices                              # list audio ports
ac setup output 11 input 0             # save port config
ac calibrate 1khz                      # interactive calibration
ac sweep level -20dbu 6dbu 1khz       # level sweep
ac sweep frequency 20hz 20khz 0dbu    # freq sweep
ac monitor thd 0dbu 1khz              # live THD monitor
ac generate sine 0dbu 1khz            # play tone
ac s f 20hz 20khz 0dbu show           # abbreviated + open plot
```

All args are positional and unit-tagged (no `--flags`). Abbreviations: `sweep`→`s`, `monitor`→`m`, `generate`→`g`, `calibrate`→`c`, `level`→`l`, `frequency`→`f`, `thd`→`t`, `sine`→`si`.

## Package layout

```
thd_tool/
  __init__.py          (empty)
  __main__.py          (dispatches to client.ac or legacy cli)
  constants.py         (shared)
  conversions.py       (shared)
  config.py            (shared)

  server/              (ZMQ server, audio backends, analysis)
  client/              (CLI parser, ZMQ client, plotting)
  ui/                  (pyqtgraph live views)
```

See `server/CLAUDE.md`, `client/CLAUDE.md`, `ui/CLAUDE.md` for subpackage docs.

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
