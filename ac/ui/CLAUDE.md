# ui/ — pyqtgraph live measurement views

## Architecture

Separate process spawned by `ac.py` via `python -m ac.ui --mode <mode> --host <host> --port <port>`. Subscribes to the ZMQ PUB socket and renders live data.

Entry point: `app.py:main()` (also called from `__main__.py`).

## File responsibilities

| File | Purpose |
|------|---------|
| `app.py` | `main()`, dark palette, `ZmqReceiver` thread, view dispatch, shared helpers |
| `spectrum.py` | `SpectrumView` — live FFT with peak hold and harmonic markers |
| `sweep.py` | `SweepView` — 3 stacked panels (THD/THD+N, gain, spectrum detail) |
| `transfer.py` | `TransferView` — 3 stacked panels (magnitude, phase, coherence) |

## Shared helpers (defined in `app.py`)

| Helper | Purpose |
|--------|---------|
| `FreqAxis(orientation)` | Factory returning AxisItem with human-readable Hz ticks (20, 100, 1k, 10k) |
| `mono_font(size=9)` | Monospace QFont for axis labels |
| `styled_plot(glw, title, ylabel, log_freq)` | Create consistently styled PlotItem |
| `add_harmonic_markers(plot, f0, sr)` | Amber dashed vertical lines at harmonics |
| `status_label()` | Top status bar widget |
| `readout_label()` | Bottom readout bar widget |

## Color palette (defined in `app.py`)

```python
BG="#0e1117"  PANEL="#161b22"  GRID="#222222"  TEXT="#aaaaaa"
BLUE="#4a9eff"  ORANGE="#e67e22"  PURPLE="#a29bfe"
RED="#e74c3c"   AMBER="#ffb43c"   GREEN="#2ecc71"  YELLOW="#f1c40f"
```

## ZmqReceiver thread

`ZmqReceiver.create()` returns a `QThread` subclass with a `frame_received(str, object)` signal. It subscribes to all topics, polls with 100 ms timeout, decodes `b"<topic> <json>"` frames, and emits them to the view's `on_frame()` slot.

## View specs

- **SpectrumView**: log-x FFT, smoothing mirrors `tui.py` (`_FALL_ALPHA=0.20`, `_PEAK_HOLD=6`, `_PEAK_DECAY=1.5`), harmonic markers via `add_harmonic_markers()`, mouse crosshair readout
- **SweepView**: 3 stacked panels (THD+THD+N overlaid, gain/frequency response, spectrum detail), log-x for freq sweep, clipped points shown as red X scatter, click-to-inspect spectrum
- **TransferView**: 3 stacked panels (magnitude centered at 0 dB, phase ±180°, coherence 0–1), crosshair readout on magnitude panel

## Dependencies

`pyqtgraph>=0.13`, `zmq`, `numpy`
